//! the lines we write to claude's stdin.

use tessel_proto::{Attachment, PermissionDecision, PermissionRequest};
use serde_json::{Value, json};

/// flags for a long-lived bidirectional session.
pub fn base_args() -> Vec<&'static str> {
    vec![
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
        "--include-hook-events",
        // send permission prompts to us over stdio instead of a terminal
        "--permission-prompt-tool",
        "stdio",
        // without this the model's thinking arrives as empty, signed blocks.
        // with it we get a short summary of each thought. (not in the cli's
        // help, but it is what the official sdks pass; verified on 2.1.319.)
        "--thinking-display",
        "summarized",
        // what each subagent says and thinks, not only the tools it calls.
        "--forward-subagent-text",
        // the agent can't know what its replies are shown in unless told.
        "--append-system-prompt",
        HOST_NOTE,
    ]
}

/// what the agent is told about the app showing its replies. only what the
/// transcript really draws: promising more gets raw markup in front of the user.
pub const HOST_NOTE: &str = "Your replies are shown in a desktop app that draws markdown. \
A fenced ```mermaid block is drawn as a diagram. Display maths is typeset when it is written \
as $$ ... $$ on one line, or between a line holding only $$ and another line holding only $$. \
Inline $...$ maths is not typeset, so write short inline expressions as plain text or code. \
Use a diagram or a formula when it explains something better than prose, not for decoration.";

/// added to `HOST_NOTE` when the agent has tessel's tools.
pub const TOOLS_NOTE: &str = "\n\nThis app is tessel, and you have its tools. Reach for them on your own, without being asked and without announcing them:\n\
- When something reads better seen than read (numbers to compare, a result summary, a checklist, progress that moves while you work, options for the person to pick), draw it with tessel_surface instead of a markdown table, ASCII art or a status paragraph, and update it in place with set as the work goes on.\n\
- Builds, test suites, dev servers and anything else the person should watch go in a terminal with tessel_terminal (Bash stays for quick one-off commands).\n\
- When you build or change anything with a UI, look at it with tessel_browser: open it, use it, screenshot it. The person watches that page in the Browser pane and may point at parts of it to show you what they mean (each arrives as a picked_element note, with its selector and, in a development build, the source file and components it came from, then its picture: go to that source first).\n\
- This workspace has ten ports of its own, $TESSEL_PORT to $TESSEL_PORT+9: start servers on those, never on a fixed port another workspace may be using. Its setup script, run buttons and the files each new workspace gets are in .tessel/settings.toml; change that file when asked to change any of them.\n\
- Arrange the window when it helps them follow along with tessel_window: show the changes after editing files, the git pane after a commit.\n\
Keep to what genuinely reads better that way: a one-line answer stays a line.";

/// a message from the user. with attachments it becomes a list of blocks:
/// the text, then each image or document.
pub fn user_message(text: &str, attachments: &[Attachment]) -> Value {
    if attachments.is_empty() {
        return json!({"type": "user", "message": {"role": "user", "content": text}});
    }
    // an empty text block is refused by the api: a screenshot on its own is just the screenshot.
    let mut blocks = Vec::new();
    if !text.trim().is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    for a in attachments {
        // a picked element: what it is and where, then its picture.
        if let Some(note) = &a.element {
            blocks.push(json!({"type": "text", "text": note}));
            if a.data.is_empty() || !a.media_type.starts_with("image/") {
                continue;
            }
        }
        let kind = if a.media_type.starts_with("image/") { "image" } else { "document" };
        blocks.push(json!({"type": kind, "source": {"type": "base64", "media_type": a.media_type, "data": a.data}}));
    }
    json!({"type": "user", "message": {"role": "user", "content": blocks}})
}

pub fn set_model(request_id: &str, model: &str) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "set_model", "model": model}})
}

/// change settings of a running agent: effort, fast mode.
pub fn apply_flag_settings(request_id: &str, settings: &Value) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "apply_flag_settings", "settings": settings}})
}

/// add (or replace) tool servers in a running agent. tessel's own server goes
/// in this way rather than `--mcp-config`: a server passed on the command line
/// makes the agent wait for *every* server, the person's broken ones included,
/// before it starts, which was twenty seconds on a real machine. added after,
/// it joins the person's in connecting in the background.
pub fn mcp_set_servers(request_id: &str, servers: &Value) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "mcp_set_servers", "servers": servers}})
}

pub fn mcp_status(request_id: &str) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "mcp_status"}})
}

/// the handshake the sdks open with. its answer lists the models on offer.
pub fn initialize(request_id: &str) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "initialize"}})
}

pub fn set_permission_mode(request_id: &str, mode: &str) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "set_permission_mode", "mode": mode}})
}

pub fn interrupt(request_id: &str) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "interrupt"}})
}

/// answer a `can_use_tool` request. on allow claude wants the (possibly
/// edited) tool input back, so an allow without edits echoes the original.
pub fn permission_response(request: &PermissionRequest, decision: &PermissionDecision) -> Value {
    let response = match decision {
        PermissionDecision::Allow { updated_input } => json!({
            "behavior": "allow",
            "updatedInput": updated_input.clone().unwrap_or_else(|| request.input.clone()),
        }),
        PermissionDecision::Deny { message } => json!({
            "behavior": "deny",
            "message": if message.is_empty() { "denied by user" } else { message.as_str() },
        }),
    };
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request.request_id.0, "response": response},
    })
}

/// reply to a control request we don't understand, so the cli never waits on us forever.
pub fn control_error(request_id: &str, message: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": request_id, "error": message},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_proto::RequestId;

    fn req() -> PermissionRequest {
        PermissionRequest {
            request_id: RequestId("r1".into()),
            tool_item: None,
            tool: "Bash".into(),
            input: json!({"command": "ls"}),
            description: None,
            is_question: false,
            plan: None,
            suggestions: vec![],
        }
    }

    #[test]
    fn a_message_with_attachments_becomes_blocks_and_one_without_stays_plain() {
        assert_eq!(user_message("hi", &[])["message"]["content"], "hi");
        let files = [
            Attachment { name: "shot.png".into(), media_type: "image/png".into(), data: "AAAA".into(), element: None },
            Attachment { name: "spec.pdf".into(), media_type: "application/pdf".into(), data: "BBBB".into(), element: None },
        ];
        let v = user_message("look", &files);
        let blocks = v["message"]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], json!({"type": "text", "text": "look"}));
        assert_eq!(blocks[1], json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}));
        assert_eq!(blocks[2]["type"], "document");
        let alone = user_message("  ", &files[..1]);
        assert_eq!(alone["message"]["content"].as_array().unwrap().len(), 1, "no empty text block: the api refuses those");
        assert!(files.iter().all(Attachment::is_supported));
        // a picked element: its note, then its picture.
        let el = Attachment { name: "button \"Go\"".into(), media_type: "image/png".into(), data: "CCCC".into(), element: Some("<picked_element>…</picked_element>".into()) };
        assert!(el.is_supported());
        let msg = user_message("make this bigger", &[el]);
        let blocks = msg["message"]["content"].as_array().unwrap();
        assert_eq!((blocks[1]["type"].as_str(), blocks[1]["text"].as_str()), (Some("text"), Some("<picked_element>…</picked_element>")));
        assert_eq!(blocks[2]["source"]["data"], "CCCC");
        assert!(!Attachment { name: "a.zip".into(), media_type: "application/zip".into(), data: String::new(), element: None }.is_supported());
    }

    #[test]
    fn model_and_mode_requests() {
        assert_eq!(set_model("r", "sonnet")["request"], json!({"subtype": "set_model", "model": "sonnet"}));
        assert_eq!(set_permission_mode("r", "plan")["request"], json!({"subtype": "set_permission_mode", "mode": "plan"}));
    }

    #[test]
    fn allow_echoes_original_input() {
        let v = permission_response(&req(), &PermissionDecision::Allow { updated_input: None });
        assert_eq!(v["response"]["request_id"], "r1");
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["response"]["updatedInput"]["command"], "ls");
    }

    #[test]
    fn allow_with_edit_sends_the_edit() {
        let d = PermissionDecision::Allow { updated_input: Some(json!({"command": "ls -la"})) };
        let v = permission_response(&req(), &d);
        assert_eq!(v["response"]["response"]["updatedInput"]["command"], "ls -la");
    }

    #[test]
    fn deny_always_has_a_message() {
        let v = permission_response(&req(), &PermissionDecision::Deny { message: String::new() });
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(v["response"]["response"]["message"], "denied by user");
    }
}
