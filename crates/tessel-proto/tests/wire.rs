//! pins the wire format. if one of these fails you changed what goes over the
//! socket: either fix it or bump the protocol version on purpose.

use tessel_proto::*;
use serde_json::{Value, json};
use uuid::Uuid;

fn round_trip<T>(value: &T) -> Value
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "round trip changed the value: {text}");
    serde_json::from_str(&text).unwrap()
}

fn sid() -> SessionId {
    SessionId(Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap())
}

fn summary() -> SessionSummary {
    SessionSummary {
        id: sid(),
        goal: "fix the flaky test".into(),
        cwd: "/work/repo".into(),
        worktree: None,
        parent: None,
        run_state: RunState::Working,
        activity: Some("running cargo test".into()),
        activity_since_ms: Some(10),
        attention: None,
        warm: true,
        background_tasks: 0,
        model: None,
        permission_mode: None,
        context_used: None,
        context_window: None,
        tasks: vec![], mcp: vec![], linked_dirs: vec![],
        cost_usd: 0.0,
        trace: vec![],
        last_seq: 7,
        created_ms: 1,
        updated_ms: 2,
    }
}

#[test]
fn hello_shape() {
    let env = Envelope::Req {
        id: 1,
        req: Request::Hello { proto_major: 0, proto_minor: 1, build: "dev".into(), client: "test".into(), auth: None },
    };
    assert_eq!(
        round_trip(&env),
        json!({"t": "req", "id": 1, "req": {"method": "hello", "proto_major": 0, "proto_minor": 1, "build": "dev", "client": "test"}})
    );
}

#[test]
fn error_response_shape() {
    let env = Envelope::Res { id: 4, ok: None, err: Some(RpcError::new(ErrorCode::NotFound, "no such session")) };
    assert_eq!(round_trip(&env), json!({"t": "res", "id": 4, "err": {"code": "not_found", "message": "no such session"}}));
}

#[test]
fn summary_shape_has_no_session_kind() {
    let v = round_trip(&summary());
    assert_eq!(
        v,
        json!({
            "id": "00000000-0000-4000-8000-000000000001",
            "goal": "fix the flaky test",
            "cwd": "/work/repo",
            "run_state": {"state": "working"},
            "activity": "running cargo test",
            "activity_since_ms": 10,
            "warm": true,
            "background_tasks": 0,
            "cost_usd": 0.0,
            "last_seq": 7,
            "created_ms": 1,
            "updated_ms": 2
        })
    );
    for banned in ["kind", "type", "mode"] {
        assert!(v.get(banned).is_none(), "a session must not carry a `{banned}`");
    }
}

#[test]
fn item_shape() {
    let item = Item {
        id: ItemId::from("toolu_1"),
        parent: None,
        seq: 9,
        order: 5,
        created_ms: 100,
        body: ItemBody::Tool {
            tool: "Bash".into(),
            input: json!({"command": "ls"}),
            status: ToolStatus::AwaitingPermission,
            output: None,
            started_ms: None,
            finished_ms: None,
        },
    };
    assert_eq!(
        round_trip(&item),
        json!({"id": "toolu_1", "seq": 9, "order": 5, "created_ms": 100, "kind": "tool", "tool": "Bash",
               "input": {"command": "ls"}, "status": "awaiting_permission"})
    );
}

#[test]
fn every_request_round_trips() {
    let cmd_id = Uuid::nil();
    let all = vec![
        Request::SessionList,
        Request::SessionCreate {
            cmd_id,
            cwd: "/work/repo".into(),
            worktree: Some(WorktreeSpec { base: Some("main".into()), name: None }),
            goal: Some("g".into()),
            prompt: Some("p".into()),
            model: None,
            parent: Some(sid()),
        },
        Request::SessionSend { cmd_id, session: sid(), text: "hi".into(), attachments: vec![] },
        Request::SessionInterrupt { cmd_id, session: sid() },
        Request::SessionSetGoal { cmd_id, session: sid(), goal: "g".into() },
        Request::SessionDelete { cmd_id, session: sid(), remove_worktree: true },
        Request::PermissionRespond {
            cmd_id,
            session: sid(),
            request_id: RequestId::from("r1"),
            decision: PermissionDecision::Allow { updated_input: Some(json!({"a": 1})) },
        },
        Request::PermissionRespond {
            cmd_id,
            session: sid(),
            request_id: RequestId::from("r2"),
            decision: PermissionDecision::Deny { message: "no".into() },
        },
        Request::MarkSeen { session: sid(), seq: 3 },
        Request::Subscribe { summary: true, detail: Some(sid()), tail: 50 },
        Request::ItemsBefore { session: sid(), before: 10, limit: 20 },
        Request::Shutdown,
    ];
    for req in all {
        round_trip(&Envelope::Req { id: 1, req });
    }
}

#[test]
fn every_event_round_trips() {
    let all = vec![
        Event::SessionUpdated { summary: summary() },
        Event::SessionRemoved { session: sid() },
        Event::ItemDelta { session: sid(), item_id: ItemId::from("m/0"), offset: 12, text: "hi".into() },
        Event::PendingChanged { session: sid(), pending: vec![] },
        Event::Lagged,
    ];
    for ev in all {
        round_trip(&Envelope::Ev { ev });
    }
}

/// a newer peer may send things this build has never heard of. none of them
/// may fail to parse.
#[test]
fn unknown_variants_are_tolerated_everywhere() {
    let env: Envelope = serde_json::from_value(json!({"t": "telepathy", "x": 1})).unwrap();
    assert_eq!(env, Envelope::Unknown);

    let env: Envelope = serde_json::from_value(json!({"t": "req", "id": 2, "req": {"method": "session_teleport", "to": "mars"}})).unwrap();
    assert_eq!(env, Envelope::Req { id: 2, req: Request::Unknown });

    let env: Envelope = serde_json::from_value(json!({"t": "ev", "ev": {"event": "session_exploded"}})).unwrap();
    assert_eq!(env, Envelope::Ev { ev: Event::Unknown });

    let state: RunState = serde_json::from_value(json!({"state": "levitating", "height": 3})).unwrap();
    assert_eq!(state, RunState::Unknown);

    let body: ItemBody = serde_json::from_value(json!({"kind": "hologram", "data": [1, 2]})).unwrap();
    assert_eq!(body, ItemBody::Unknown);

    let ev: AgentEvent = serde_json::from_value(json!({"type": "mind_meld"})).unwrap();
    assert_eq!(ev, AgentEvent::Unknown);

    let status: ToolStatus = serde_json::from_value(json!("vaporised")).unwrap();
    assert_eq!(status, ToolStatus::Unknown);
}

/// extra fields from a newer peer are ignored, not rejected.
#[test]
fn unknown_fields_are_ignored() {
    let mut v = serde_json::to_value(summary()).unwrap();
    v["shiny_new_field"] = json!({"nested": true});
    let back: SessionSummary = serde_json::from_value(v).unwrap();
    assert_eq!(back, summary());
}

#[test]
fn version_compatibility_is_major_only() {
    assert!(compatible(PROTO_MAJOR));
    assert!(!compatible(PROTO_MAJOR + 1));
}
