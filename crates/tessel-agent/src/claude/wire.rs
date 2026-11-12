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
pub const TOOLS_NOTE: &str = "\n\nYou also have tessel's tools. tessel_surface draws live UI in this chat (progress, status cards, choices, results) that you update in place as work goes on; prefer it to repeating status in prose for anything that changes. tessel_window runs anything a person can click in the app: split panes, show changes or files, switch theme. tessel_terminal runs things the person should watch, like a dev server, in a terminal pane (Bash is still for one-off commands). Use them when they help the person, not for their own sake.";

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
            Attachment { name: "shot.png".into(), media_type: "image/png".into(), data: "AAAA".into() },
            Attachment { name: "spec.pdf".into(), media_type: "application/pdf".into(), data: "BBBB".into() },
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
        assert!(!Attachment { name: "a.zip".into(), media_type: "application/zip".into(), data: String::new() }.is_supported());
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
