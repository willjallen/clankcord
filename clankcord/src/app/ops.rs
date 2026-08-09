//! Process-level telemetry: /proc, cgroup, and file-descriptor facts.
//!
//! This is operating-system observation, not a timeline read model; it lives
//! at the application layer and is composed into operator payloads by the
//! surfaces that serve them.

use std::collections::BTreeMap;
use std::fs;

use serde_json::{Map, Value, json};

pub fn process_load_payload() -> Value {
    let status = proc_status_fields();
    let meminfo = proc_meminfo_fields();
    json!({
        "pid": std::process::id(),
        "threads": status.get("Threads").copied(),
        "openFileDescriptors": open_file_descriptor_count(),
        "loadAverage": proc_load_average_payload(),
        "memory": {
            "rssBytes": status.get("VmRSS").copied(),
            "vmSizeBytes": status.get("VmSize").copied(),
            "vmPeakBytes": status.get("VmPeak").copied(),
            "hostTotalBytes": meminfo.get("MemTotal").copied(),
            "hostAvailableBytes": meminfo.get("MemAvailable").copied(),
            "cgroupCurrentBytes": read_u64_file("/sys/fs/cgroup/memory.current"),
            "cgroupMaxBytes": cgroup_memory_max(),
        },
        "cpu": {
            "process": proc_self_stat_payload(),
            "cgroup": cgroup_cpu_stat_payload(),
        },
    })
}

fn proc_status_fields() -> BTreeMap<String, u64> {
    let mut fields = BTreeMap::new();
    let Ok(content) = fs::read_to_string("/proc/self/status") else {
        return fields;
    };
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key == "Threads" {
            if let Some(value) = value.split_whitespace().next().and_then(parse_u64) {
                fields.insert(key.to_string(), value);
            }
            continue;
        }
        if key.starts_with("Vm") {
            if let Some(bytes) = parse_kb_value(value) {
                fields.insert(key.to_string(), bytes);
            }
        }
    }
    fields
}

fn proc_meminfo_fields() -> BTreeMap<String, u64> {
    let mut fields = BTreeMap::new();
    let Ok(content) = fs::read_to_string("/proc/meminfo") else {
        return fields;
    };
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if matches!(key, "MemTotal" | "MemAvailable") {
            if let Some(bytes) = parse_kb_value(value) {
                fields.insert(key.to_string(), bytes);
            }
        }
    }
    fields
}

fn proc_load_average_payload() -> Value {
    let Ok(content) = fs::read_to_string("/proc/loadavg") else {
        return json!({});
    };
    let parts = content.split_whitespace().collect::<Vec<_>>();
    let (runnable, total_threads) = parts
        .get(3)
        .and_then(|value| value.split_once('/'))
        .map(|(running, total)| (parse_u64(running), parse_u64(total)))
        .unwrap_or((None, None));
    json!({
        "oneMinute": parts.first().and_then(|value| value.parse::<f64>().ok()),
        "fiveMinute": parts.get(1).and_then(|value| value.parse::<f64>().ok()),
        "fifteenMinute": parts.get(2).and_then(|value| value.parse::<f64>().ok()),
        "runnableThreads": runnable,
        "totalThreads": total_threads,
        "lastPid": parts.get(4).and_then(|value| parse_u64(value)),
    })
}

fn proc_self_stat_payload() -> Value {
    let Ok(content) = fs::read_to_string("/proc/self/stat") else {
        return json!({});
    };
    let Some(close_comm) = content.rfind(')') else {
        return json!({});
    };
    let fields = content[close_comm + 1..]
        .split_whitespace()
        .collect::<Vec<_>>();
    let user_ticks = fields.get(11).and_then(|value| parse_u64(value));
    let system_ticks = fields.get(12).and_then(|value| parse_u64(value));
    json!({
        "userTicks": user_ticks,
        "systemTicks": system_ticks,
        "totalTicks": user_ticks.zip(system_ticks).map(|(user, system)| user + system),
        "startTimeTicks": fields.get(19).and_then(|value| parse_u64(value)),
    })
}

fn cgroup_cpu_stat_payload() -> Value {
    let Ok(content) = fs::read_to_string("/sys/fs/cgroup/cpu.stat") else {
        return json!({});
    };
    let mut object = Map::new();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(value) = parts.next().and_then(parse_u64) else {
            continue;
        };
        object.insert(key.to_string(), json!(value));
    }
    Value::Object(object)
}

fn cgroup_memory_max() -> Value {
    let Ok(raw) = fs::read_to_string("/sys/fs/cgroup/memory.max") else {
        return Value::Null;
    };
    let value = raw.trim();
    if value == "max" {
        Value::Null
    } else {
        parse_u64(value).map_or(Value::Null, |value| json!(value))
    }
}

fn read_u64_file(path: &str) -> Option<u64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| parse_u64(content.trim()))
}

fn open_file_descriptor_count() -> Option<usize> {
    fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.filter_map(std::result::Result::ok).count())
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

fn parse_kb_value(value: &str) -> Option<u64> {
    value
        .split_whitespace()
        .next()
        .and_then(parse_u64)
        .map(|kb| kb.saturating_mul(1024))
}
