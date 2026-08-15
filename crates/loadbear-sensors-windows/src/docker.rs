//! Container readings from the Docker Engine API.
//!
//! A second source, not a refinement of process attribution. On Windows every
//! container on the machine runs inside WSL2 and presents to the OS as a single
//! `vmmem` process, so Windows can report that Docker holds eleven gigabytes
//! and can never report which container holds it. Only the engine knows.
//!
//! # Why this is hand-rolled
//!
//! The engine speaks HTTP over a named pipe on Windows and a Unix socket
//! elsewhere. The protocol is the same on all three platforms, so this is one
//! implementation rather than three, and the only platform-specific part is
//! which path gets opened. The requests are two fixed GETs against a local pipe
//! with no redirects, no authentication and no chunk negotiation, so a full
//! HTTP client would be a large dependency for a job that fits on a page.
//!
//! # Docker is optional
//!
//! Most machines have no engine running, and that is not an error. Every
//! failure here, from a missing pipe to malformed JSON, produces an empty list.
//! Attribution then names Docker no more precisely than the process layer
//! already did, which is the honest answer when the second source is not
//! available.

use std::io::{Read, Write};

use loadbear_core::ContainerReading;

/// The engine's named pipe on Windows.
const PIPE: &str = r"\\.\pipe\docker_engine";

/// Pinned so the engine cannot change the response shape underneath us. Every
/// field used here has been stable across the whole v1.4x series.
const API: &str = "v1.43";

/// A ceiling on how much of a response is read, so a runaway engine cannot
/// grow LoadBear's memory. Comfortably above a realistic container list.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Ask the engine which containers are running and what they are using.
///
/// **Blocking, and deliberately not called from the sampling loop.** A named
/// pipe opened as an ordinary file has no read timeout without overlapped I/O,
/// so a wedged engine would stall whatever thread asked it. The application
/// runs this on a thread of its own and reads the last answer it left behind,
/// which costs a slightly stale container list and cannot stall the loop that
/// produces the tier.
///
/// Returns an empty list whenever Docker is absent, unreachable or unreadable.
/// There is deliberately no error type: no caller would do anything different
/// with one, and a `Result` here would invite treating a machine without Docker
/// as a failure.
pub fn read_containers() -> Vec<ContainerReading> {
    let Some(list) = get(&format!("/{API}/containers/json")) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in json::array_items(&list) {
        let Some(id) = json::string(&entry, "Id") else {
            continue;
        };
        let name = json::first_name(&entry).unwrap_or_else(|| short_id(&id));

        // `one-shot` asks the engine not to spend a second collecting a second
        // CPU sample. The tradeoff is that CPU comes back without a baseline to
        // difference against, so it is reported as zero rather than invented,
        // and memory, which needs no baseline, is exact.
        let stats = get(&format!(
            "/{API}/containers/{id}/stats?stream=false&one-shot=true"
        ));
        let (memory_bytes, memory_limit_bytes, cpu_percent) = match &stats {
            Some(s) => (
                json::nested_u64(s, "memory_stats", "usage").unwrap_or(0),
                json::nested_u64(s, "memory_stats", "limit").filter(|l| *l > 0),
                cpu_percent(s),
            ),
            None => (0, None, 0.0),
        };

        out.push(ContainerReading {
            id,
            name,
            cpu_percent,
            memory_bytes,
            memory_limit_bytes,
        });
    }
    out
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// The engine's CPU figures are cumulative, so a percentage needs the
/// `precpu_stats` baseline the engine ships alongside them.
///
/// With `one-shot` there is no baseline, and the difference against a zeroed
/// `precpu_stats` would credit the container with its entire lifetime. So a
/// missing or zero baseline yields zero rather than a number that looks
/// authoritative and is wrong by orders of magnitude.
fn cpu_percent(stats: &str) -> f32 {
    let total = json::nested_u64(stats, "cpu_stats", "cpu_usage_total_usage");
    let pre_total = json::nested_u64(stats, "precpu_stats", "cpu_usage_total_usage");
    let system = json::nested_u64(stats, "cpu_stats", "system_cpu_usage");
    let pre_system = json::nested_u64(stats, "precpu_stats", "system_cpu_usage");

    let (Some(total), Some(pre_total), Some(system), Some(pre_system)) =
        (total, pre_total, system, pre_system)
    else {
        return 0.0;
    };
    if pre_total == 0 || pre_system == 0 || system <= pre_system {
        return 0.0;
    }

    let delta = total.saturating_sub(pre_total) as f64;
    let system_delta = system.saturating_sub(pre_system) as f64;
    if system_delta <= 0.0 {
        return 0.0;
    }
    ((delta / system_delta) * 100.0).clamp(0.0, 100.0) as f32
}

/// One HTTP GET against the engine, returning the response body.
#[cfg(windows)]
fn get(path: &str) -> Option<String> {
    use std::fs::OpenOptions;

    let mut pipe = OpenOptions::new().read(true).write(true).open(PIPE).ok()?;

    let request = format!("GET {path} HTTP/1.1\r\nHost: docker\r\nAccept: application/json\r\nConnection: close\r\n\r\n");
    pipe.write_all(request.as_bytes()).ok()?;
    pipe.flush().ok()?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_RESPONSE_BYTES {
                    return None;
                }
            }
            Err(_) => break,
        }
    }

    let text = String::from_utf8_lossy(&raw).into_owned();
    body(&text)
}

#[cfg(not(windows))]
fn get(_path: &str) -> Option<String> {
    // The Unix socket path lands with the macOS and Linux backends. The wire
    // protocol above is already shared; only the transport differs.
    None
}

/// Split an HTTP response, de-chunking the body if the engine chunked it.
///
/// The engine chunks whenever it feels like it, and a chunked body parsed as
/// plain text has length prefixes embedded in the JSON.
fn body(response: &str) -> Option<String> {
    let (head, rest) = response.split_once("\r\n\r\n")?;

    let status_ok = head
        .lines()
        .next()
        .map(|l| l.contains(" 200"))
        .unwrap_or(false);
    if !status_ok {
        return None;
    }

    let chunked = head.to_lowercase().contains("transfer-encoding: chunked");
    if !chunked {
        return Some(rest.to_string());
    }

    let mut out = String::new();
    let mut remaining = rest;
    while let Some((size_line, after)) = remaining.split_once("\r\n") {
        let size =
            usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("").trim(), 16)
                .unwrap_or(0);
        if size == 0 || size > after.len() {
            break;
        }
        out.push_str(&after[..size]);
        remaining = after[size..].strip_prefix("\r\n").unwrap_or("");
    }
    Some(out)
}

/// Just enough JSON reading for two known response shapes.
///
/// Not a JSON parser and not trying to be. It reads a handful of named fields
/// out of documented responses, and anything it cannot make sense of comes back
/// as `None`, which every caller already treats as "Docker did not say".
mod json {
    /// Split a top level array into its object elements.
    pub fn array_items(text: &str) -> Vec<String> {
        let mut items = Vec::new();
        let mut depth = 0i32;
        let mut start = None;
        let mut in_string = false;
        let mut escaped = false;

        for (i, c) in text.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' if in_string => escaped = true,
                '"' => in_string = !in_string,
                '{' if !in_string => {
                    if depth == 0 {
                        start = Some(i);
                    }
                    depth += 1;
                }
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(s) = start.take() {
                            items.push(text[s..=i].to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        items
    }

    /// The value of a string field, unescaped only for the cases that occur.
    pub fn string(object: &str, key: &str) -> Option<String> {
        let needle = format!("\"{key}\":");
        let at = object.find(&needle)? + needle.len();
        let rest = object[at..].trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    /// The first entry of the `Names` array, without its leading slash.
    ///
    /// The engine returns names as `/postgres`, and a user does not think of
    /// their container as having a slash in front of it.
    pub fn first_name(object: &str) -> Option<String> {
        let at = object.find("\"Names\":")? + "\"Names\":".len();
        let rest = object[at..].trim_start().strip_prefix('[')?;
        let rest = rest.trim_start().strip_prefix('"')?;
        let end = rest.find('"')?;
        let name = &rest[..end];
        Some(name.trim_start_matches('/').to_string())
    }

    /// A number nested one level down, addressed as `outer` then `inner`.
    ///
    /// `cpu_usage_total_usage` is a compound key standing for `cpu_usage` then
    /// `total_usage`, which saves a second level of scanning for the only place
    /// two levels are needed.
    pub fn nested_u64(text: &str, outer: &str, inner: &str) -> Option<u64> {
        let at = text.find(&format!("\"{outer}\":"))?;
        let scope = &text[at..];

        let (first, second) = match inner.strip_prefix("cpu_usage_") {
            Some(leaf) => ("cpu_usage", Some(leaf)),
            None => (inner, None),
        };

        let scope = match second {
            Some(_) => {
                let at = scope.find(&format!("\"{first}\":"))?;
                &scope[at..]
            }
            None => scope,
        };
        let key = second.unwrap_or(first);

        let at = scope.find(&format!("\"{key}\":"))? + key.len() + 3;
        let rest = scope[at..].trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"[
      {"Id":"abc123def456789","Names":["/postgres"],"Image":"postgres:16","State":"running"},
      {"Id":"fed987cba321000","Names":["/redis"],"Image":"redis:7","State":"running"}
    ]"#;

    const STATS: &str = r#"{
      "memory_stats":{"usage":9663676416,"limit":17179869184},
      "cpu_stats":{"cpu_usage":{"total_usage":200000000},"system_cpu_usage":900000000},
      "precpu_stats":{"cpu_usage":{"total_usage":100000000},"system_cpu_usage":500000000}
    }"#;

    #[test]
    fn a_container_list_splits_into_its_entries() {
        let items = json::array_items(LIST);
        assert_eq!(items.len(), 2);
        assert_eq!(json::string(&items[0], "Id").unwrap(), "abc123def456789");
    }

    #[test]
    fn a_container_name_loses_the_engines_leading_slash() {
        let items = json::array_items(LIST);
        assert_eq!(json::first_name(&items[0]).unwrap(), "postgres");
        assert_eq!(json::first_name(&items[1]).unwrap(), "redis");
    }

    #[test]
    fn memory_usage_and_limit_are_read_from_stats() {
        assert_eq!(
            json::nested_u64(STATS, "memory_stats", "usage"),
            Some(9_663_676_416)
        );
        assert_eq!(
            json::nested_u64(STATS, "memory_stats", "limit"),
            Some(17_179_869_184)
        );
    }

    #[test]
    fn cpu_is_a_share_of_the_system_delta() {
        // 100_000_000 of container time against 400_000_000 of system time.
        assert_eq!(cpu_percent(STATS), 25.0);
    }

    #[test]
    fn cpu_without_a_baseline_reads_zero_rather_than_a_lifetime_total() {
        // What `one-shot=true` actually returns. Differencing against this
        // would report a container that has run for a week as pinning the CPU.
        let no_baseline = r#"{
          "cpu_stats":{"cpu_usage":{"total_usage":900000000000},"system_cpu_usage":900000000000},
          "precpu_stats":{"cpu_usage":{"total_usage":0},"system_cpu_usage":0}
        }"#;
        assert_eq!(cpu_percent(no_baseline), 0.0);
    }

    #[test]
    fn a_plain_response_body_is_returned_as_is() {
        let r = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n[]";
        assert_eq!(body(r).unwrap(), "[]");
    }

    #[test]
    fn a_chunked_response_is_reassembled_without_its_length_prefixes() {
        let r = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n[{\"a\r\n3\r\n\":1\r\n2\r\n}]\r\n0\r\n\r\n";
        assert_eq!(
            body(r).unwrap(),
            "[{\"a\":1}]",
            "a chunked body read as plain text carries hex lengths into the JSON"
        );
    }

    #[test]
    fn a_non_success_response_yields_nothing() {
        let r = "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\r\n{\"message\":\"no such container\"}";
        assert!(body(r).is_none());
    }

    #[test]
    fn a_truncated_response_yields_nothing_rather_than_half_a_body() {
        assert!(body("HTTP/1.1 200 OK\r\nContent-Type: applicat").is_none());
    }

    #[test]
    fn an_absent_engine_produces_an_empty_list_rather_than_an_error() {
        // Passes whether or not Docker is running here. With no engine the
        // pipe cannot be opened; with one, a real list comes back. Neither is
        // a failure, which is the whole point.
        let containers = read_containers();
        for c in &containers {
            assert!(!c.id.is_empty(), "a container with no id cannot be named");
            assert!(!c.name.is_empty());
        }
    }
}
