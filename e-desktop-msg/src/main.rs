//! `e-desktop-msg`: console client for the e-desktop IPC endpoint (lane: persistence-ipc).
//! The GUI executable uses the Windows GUI subsystem and prints nothing in a terminal.
use std::io::{BufRead, BufReader, Write};

use serde_json::{Value, json};

const USAGE: &str = "用法：e-desktop-msg [--json] <请求>

请求：
  action '<命令 JSON>'           执行布局命令，例如 '{\"type\":\"focusDirection\",\"direction\":\"left\"}'
  shortcut-action '<动作 JSON>'  执行快捷键动作，例如 '{\"type\":\"overview\"}'
  snapshot | windows | pages | focused-window | config
                                 查询当前状态
  event-stream                   状态每次变化输出一行

选项：
  --json  原样输出 e-desktop 返回的 JSON 行";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Action,
    Snapshot,
    Windows,
    Pages,
    FocusedWindow,
    Config,
    EventStream,
}

/// `(raw JSON output, verb, request line)` from the arguments after the program name.
fn parse_args(args: &[String]) -> Result<(bool, Verb, Value), String> {
    let json = args.iter().any(|a| a == "--json");
    let rest: Vec<&str> = args
        .iter()
        .filter(|a| *a != "--json")
        .map(String::as_str)
        .collect();
    let payload = |text: &str| {
        serde_json::from_str::<Value>(text).map_err(|e| format!("参数不是有效的 JSON：{e}"))
    };
    let (verb, request) = match rest.as_slice() {
        ["action", command] => (Verb::Action, json!({ "action": payload(command)? })),
        ["shortcut-action", action] => {
            (Verb::Action, json!({ "shortcutAction": payload(action)? }))
        }
        ["snapshot"] => (Verb::Snapshot, json!({ "query": "snapshot" })),
        ["windows"] => (Verb::Windows, json!({ "query": "windows" })),
        ["pages"] => (Verb::Pages, json!({ "query": "pages" })),
        ["focused-window"] => (Verb::FocusedWindow, json!({ "query": "focusedWindow" })),
        ["config"] => (Verb::Config, json!({ "query": "config" })),
        ["event-stream"] => (Verb::EventStream, json!({ "eventStream": true })),
        _ => return Err(USAGE.into()),
    };
    Ok((json, verb, request))
}

fn window_line(window: &Value) -> String {
    let mut line = format!(
        "{}\t{}\t{}",
        window["native"]["id"].as_str().unwrap_or_default(),
        window["native"]["appName"].as_str().unwrap_or_default(),
        window["native"]["title"].as_str().unwrap_or_default()
    );
    if window["floating"] == true {
        line.push_str("\t[浮动]");
    }
    if window["fullscreen"] == true {
        line.push_str("\t[全屏]");
    }
    line
}

/// Human-readable output for an `ok` value.
fn render(verb: Verb, value: &Value) -> String {
    match verb {
        Verb::Action | Verb::EventStream => String::new(),
        Verb::Windows => value
            .as_array()
            .into_iter()
            .flatten()
            .map(window_line)
            .collect::<Vec<_>>()
            .join("\n"),
        Verb::FocusedWindow if value.is_null() => "无聚焦窗口".into(),
        Verb::FocusedWindow => window_line(value),
        Verb::Pages => {
            let mut lines = vec![];
            for monitor in value.as_array().into_iter().flatten() {
                lines.push(format!(
                    "显示器 {} ({})",
                    monitor["monitorName"].as_str().unwrap_or_default(),
                    monitor["monitorId"].as_str().unwrap_or_default()
                ));
                for page in monitor["pages"].as_array().into_iter().flatten() {
                    let columns = page["columns"].as_array().map_or(0, Vec::len);
                    let floating = page["floatingWindows"].as_array().map_or(0, Vec::len);
                    lines.push(format!(
                        "  {} {} ({})：{columns} 列，{floating} 个浮动窗口",
                        if page["id"] == monitor["activePage"] {
                            "*"
                        } else {
                            " "
                        },
                        page["name"].as_str().unwrap_or_default(),
                        page["id"].as_str().unwrap_or_default()
                    ));
                }
            }
            lines.join("\n")
        }
        Verb::Snapshot | Verb::Config => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

fn event_line(snapshot: &Value) -> String {
    let focused = snapshot["focusedWindow"].as_str();
    let title = snapshot["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|w| w["native"]["id"].as_str() == focused)
        .map_or("无", |w| w["native"]["title"].as_str().unwrap_or_default());
    format!(
        "{}，窗口 {} 个，焦点：{title}",
        if snapshot["enabled"] == true {
            "平铺中"
        } else {
            "已暂停"
        },
        snapshot["windows"].as_array().map_or(0, Vec::len)
    )
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let (json, verb, request) = parse_args(&args)?;
    let stream = e_desktop::ipc::connect().map_err(|e| {
        format!(
            "无法连接 e-desktop（{}）：{e}\n请确认 e-desktop 正在运行。",
            e_desktop::ipc::endpoint().display()
        )
    })?;
    (&stream)
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut lines = BufReader::new(&stream).lines();
    let reply = lines
        .next()
        .ok_or("e-desktop 关闭了连接。")?
        .map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout().lock();
    let value: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    if json {
        writeln!(stdout, "{reply}").map_err(|e| e.to_string())?;
    }
    if let Some(issue) = value.get("error") {
        return Err(issue["message"].as_str().unwrap_or(&reply).to_owned());
    }
    if !json {
        let text = render(verb, &value["ok"]);
        if !text.is_empty() {
            writeln!(stdout, "{text}").map_err(|e| e.to_string())?;
        }
    }
    if verb == Verb::EventStream {
        for line in lines {
            let line = line.map_err(|e| e.to_string())?;
            if json {
                writeln!(stdout, "{line}")
            } else {
                let event: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                writeln!(stdout, "{}", event_line(&event["snapshot"]))
            }
            .and_then(|()| stdout.flush())
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &[&str]) -> Vec<String> {
        text.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arguments_become_one_request_line() {
        let (json, verb, request) = parse_args(&args(&[
            "action",
            r#"{"type":"focusDirection","direction":"left"}"#,
        ]))
        .unwrap();
        assert!(!json);
        assert_eq!(verb, Verb::Action);
        assert_eq!(
            request.to_string(),
            r#"{"action":{"direction":"left","type":"focusDirection"}}"#
        );
        // The line the server parses is the same request.
        assert!(e_desktop::ipc::parse_request(&request.to_string()).is_ok());
        let (json, verb, request) = parse_args(&args(&["--json", "focused-window"])).unwrap();
        assert!(json);
        assert_eq!(verb, Verb::FocusedWindow);
        assert_eq!(request, json!({"query": "focusedWindow"}));
        for (words, expected) in [
            (
                &["shortcut-action", r#"{"type":"overview"}"#][..],
                json!({"shortcutAction": {"type": "overview"}}),
            ),
            (&["windows"][..], json!({"query": "windows"})),
            (&["pages", "--json"][..], json!({"query": "pages"})),
            (&["event-stream"][..], json!({"eventStream": true})),
        ] {
            let request = parse_args(&args(words)).unwrap().2;
            assert_eq!(request, expected);
            assert!(e_desktop::ipc::parse_request(&request.to_string()).is_ok());
        }
        for words in [
            &[][..],
            &["action"][..],
            &["action", "{"][..],
            &["windows", "extra"][..],
            &["unknown"][..],
        ] {
            assert!(parse_args(&args(words)).is_err(), "{words:?}");
        }
    }

    #[test]
    fn human_output_lists_windows_pages_and_events() {
        let window = json!({
            "native": {"id": "win-1", "appName": "code.exe", "title": "main.rs"},
            "floating": true,
            "fullscreen": false
        });
        assert_eq!(
            render(Verb::Windows, &json!([window])),
            "win-1\tcode.exe\tmain.rs\t[浮动]"
        );
        assert_eq!(render(Verb::FocusedWindow, &Value::Null), "无聚焦窗口");
        assert_eq!(render(Verb::Action, &Value::Null), "");
        let pages = json!([{
            "monitorId": "m1",
            "monitorName": "DISPLAY1",
            "activePage": "p2",
            "pages": [
                {"id": "p1", "name": "Desktop 1", "columns": [{}, {}], "floatingWindows": []},
                {"id": "p2", "name": "Desktop 2", "columns": [], "floatingWindows": ["w"]}
            ]
        }]);
        assert_eq!(
            render(Verb::Pages, &pages),
            "显示器 DISPLAY1 (m1)\n    Desktop 1 (p1)：2 列，0 个浮动窗口\n  * Desktop 2 (p2)：0 列，1 个浮动窗口"
        );
        let snapshot = json!({"enabled": true, "focusedWindow": "win-1", "windows": [window]});
        assert_eq!(event_line(&snapshot), "平铺中，窗口 1 个，焦点：main.rs");
    }
}
