/// Linux startup preflight checks.
///
/// Verifies kernel limits won't silently break a benchmark run.
/// On non-Linux platforms, all checks are skipped.

/// Compute the number of file descriptors required for a test run.
#[cfg(any(target_os = "linux", test))]
pub fn required_fds(total_connections: u32, worker_count: u32) -> u32 {
    total_connections + (worker_count * 3) + 128
}

#[cfg(not(target_os = "linux"))]
pub fn run_preflight(
    _total_connections: u32,
    _worker_count: u32,
    _json: bool,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn run_preflight(
    total_connections: u32,
    worker_count: u32,
    json: bool,
) -> anyhow::Result<()> {
    use anyhow::{bail, Context};

    macro_rules! out {
        ($($arg:tt)*) => {
            if json { eprintln!($($arg)*); } else { println!($($arg)*); }
        };
    }

    let required = required_fds(total_connections, worker_count);
    let mut any_fail = false;

    out!("Linux startup checks:");

    // 1. nofile soft/hard from /proc/self/limits
    let (nofile_tag, nofile_fail) = match parse_nofile_limits() {
        Ok((soft, hard)) => {
            let status = if soft < required as u64 {
                any_fail = true;
                format!("FAIL (required: {})", required)
            } else if hard < (required as u64) * 2 {
                format!("WARN (hard limit < 2x required {})", required * 2)
            } else {
                format!("OK  (required: {})", required)
            };
            let fail = soft < required as u64;
            out!("  nofile soft/hard: {} / {}   {}", soft, hard, status);
            (status, fail)
        }
        Err(e) => {
            out!("  nofile soft/hard: unable to read   WARN ({})", e);
            (String::new(), false)
        }
    };
    let _ = nofile_tag;

    // 2. ip_local_port_range
    match parse_port_range() {
        Ok((low, high)) => {
            let range = high - low;
            let status = if range < (total_connections * 2) as u32 {
                "WARN"
            } else {
                "OK"
            };
            out!("  ip_local_port_range: {} {}    {}", low, high, status);
        }
        Err(e) => {
            out!("  ip_local_port_range: unable to read   WARN ({})", e);
        }
    }

    // 3. tcp_tw_reuse
    match parse_tcp_tw_reuse() {
        Ok(val) => {
            let status = if val == 1 || val == 2 { "OK" } else { "WARN" };
            out!("  tcp_tw_reuse: {}                     {}", val, status);
        }
        Err(e) => {
            out!("  tcp_tw_reuse: unable to read         WARN ({})", e);
        }
    }

    out!();

    if nofile_fail {
        bail!(
            "nofile soft limit is too low for {} connections + {} workers (required: {} FDs)",
            total_connections,
            worker_count,
            required
        );
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn parse_nofile_limits() -> anyhow::Result<(u64, u64)> {
    use anyhow::Context;

    let contents = std::fs::read_to_string("/proc/self/limits")
        .context("failed to read /proc/self/limits")?;

    for line in contents.lines() {
        if line.starts_with("Max open files") {
            // Format: "Max open files            65535                65535                files"
            let rest = &line["Max open files".len()..];
            let nums: Vec<&str> = rest.split_whitespace().collect();
            if nums.len() >= 2 {
                let soft = if nums[0] == "unlimited" {
                    u64::MAX
                } else {
                    nums[0]
                        .parse::<u64>()
                        .context("failed to parse nofile soft limit")?
                };
                let hard = if nums[1] == "unlimited" {
                    u64::MAX
                } else {
                    nums[1]
                        .parse::<u64>()
                        .context("failed to parse nofile hard limit")?
                };
                return Ok((soft, hard));
            }
        }
    }

    anyhow::bail!("'Max open files' line not found in /proc/self/limits")
}

#[cfg(target_os = "linux")]
fn parse_port_range() -> anyhow::Result<(u32, u32)> {
    use anyhow::Context;

    let contents = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .context("failed to read /proc/sys/net/ipv4/ip_local_port_range")?;

    let parts: Vec<&str> = contents.trim().split_whitespace().collect();
    if parts.len() >= 2 {
        let low = parts[0]
            .parse::<u32>()
            .context("failed to parse port range low")?;
        let high = parts[1]
            .parse::<u32>()
            .context("failed to parse port range high")?;
        return Ok((low, high));
    }

    anyhow::bail!("unexpected format in ip_local_port_range")
}

#[cfg(target_os = "linux")]
fn parse_tcp_tw_reuse() -> anyhow::Result<u32> {
    use anyhow::Context;

    let contents = std::fs::read_to_string("/proc/sys/net/ipv4/tcp_tw_reuse")
        .context("failed to read /proc/sys/net/ipv4/tcp_tw_reuse")?;

    contents
        .trim()
        .parse::<u32>()
        .context("failed to parse tcp_tw_reuse value")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_required_fds_calculation() {
        // 1024 connections + (8 workers * 3) + 128 = 1024 + 24 + 128 = 1176
        assert_eq!(required_fds(1024, 8), 1176);

        // 10 connections + (2 workers * 3) + 128 = 10 + 6 + 128 = 144
        assert_eq!(required_fds(10, 2), 144);

        // 0 connections + (1 worker * 3) + 128 = 0 + 3 + 128 = 131
        assert_eq!(required_fds(0, 1), 131);
    }

    #[test]
    fn test_preflight_does_not_fail_on_current_system() {
        // Dev machines should have reasonable limits; this must not error.
        run_preflight(10, 2, false).expect("preflight should pass with low connection count");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_nofile_limits() {
        let (soft, hard) = parse_nofile_limits().expect("should parse /proc/self/limits");
        assert!(soft > 0, "soft limit should be positive");
        assert!(hard >= soft, "hard limit should be >= soft limit");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_port_range() {
        let (low, high) = parse_port_range().expect("should parse ip_local_port_range");
        assert!(low < high, "low port should be less than high port");
        assert!(low >= 1024, "low port should be >= 1024");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_tcp_tw_reuse() {
        let val = parse_tcp_tw_reuse().expect("should parse tcp_tw_reuse");
        assert!(val <= 2, "tcp_tw_reuse should be 0, 1, or 2");
    }
}
