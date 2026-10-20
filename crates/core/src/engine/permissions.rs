//! A helper reads its parent's live ceiling without taking another state lock.
use super::*;
pub(super) use crate::tools::admission::PermissionCeiling;

impl Inner {
    pub(super) fn action_admission(
        &self,
        st: &RtState,
    ) -> crate::tools::admission::ActionAdmission {
        let mut ceilings = vec![st.permission_ceiling.clone()];
        if st.info.kind == SessionKind::Subagent && st.internal.subagent.is_some() {
            let Some((_, parent)) = st
                .parent_ceiling
                .as_ref()
                .filter(|(id, _)| st.info.parent_id.as_ref() == Some(id) && id != &st.info.id)
            else {
                return crate::tools::admission::ActionAdmission::denied();
            };
            ceilings.push(parent.clone());
        }
        crate::tools::admission::ActionAdmission::capture(ceilings, st.cancel.clone())
    }

    fn own_permission(&self, st: &RtState) -> PermissionMode {
        let chosen = match (is_room(&st.info), self.speaker_bot(st)) {
            (true, Some(b)) => b.permission,
            _ => st.info.permission,
        };
        self.config.read().sandbox_permission(chosen)
    }

    /// Called under this session's state lock whenever its authority changes.
    pub(super) fn publish_permission(&self, st: &RtState) {
        st.permission_ceiling.store(self.own_permission(st));
    }

    /// Channels use the speaking bot's permission; helpers also obey their
    /// parent's current ceiling. No session lookup or parent lock here.
    pub(super) fn effective_permission(&self, st: &RtState) -> PermissionMode {
        let own = self.own_permission(st);
        if st.info.kind == SessionKind::Subagent && st.internal.subagent.is_some() {
            let parent = st
                .parent_ceiling
                .as_ref()
                .filter(|(id, _)| st.info.parent_id.as_ref() == Some(id) && id != &st.info.id)
                .map_or(PermissionMode::ReadOnly, |(_, ceiling)| ceiling.load());
            return crate::tools::cap_permission(own, parent);
        }
        own
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn fake_claude_child_can_act_under_a_busy_parent_but_not_a_downgraded_one() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let cli = work.path().join("claude-fixture");
        std::fs::write(&cli, r#"#!/usr/bin/env python3
import json, sys
args = sys.argv[1:]
if args[:1] == ['--version']:
    print('9.9.9 (Claude Code)'); sys.exit(0)
if args[:2] == ['auth', 'status']:
    print(json.dumps({'loggedIn':True,'authMethod':'claude.ai'})); sys.exit(0)
def emit(x):
    print(json.dumps(x), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    if m.get('type') == 'control_request' and m['request']['subtype'] == 'initialize':
        hook = m['request']['hooks']['PreToolUse'][0]['hookCallbackIds'][0]
        emit({'type':'control_response','response':{'subtype':'success','request_id':m['request_id'],'response':{'models':[]}}})
    if m.get('type') != 'user': continue
    emit({'type':'system','subtype':'init','session_id':'fixture-session','model':'haiku','mcp_servers':[{'name':'tessera','status':'connected'}]})
    emit({'type':'user','isReplay':True,'uuid':m.get('uuid'),'message':m['message'],'session_id':'fixture-session'})
    inp = {'command':'printf fixture','description':'Fixture command'}
    emit({'type':'assistant','message':{'id':'fixture-action','model':'haiku','content':[{'type':'tool_use','id':'fixture-call','name':'Bash','input':inp}], 'usage':{'input_tokens':1,'output_tokens':1}}})
    emit({'type':'control_request','request_id':'fixture-hook','request':{'subtype':'hook_callback','callback_id':hook,'tool_use_id':'fixture-call','input':{'hook_event_name':'PreToolUse','tool_name':'Bash','tool_input':inp,'tool_use_id':'fixture-call'}}})
    for response in sys.stdin:
        r = json.loads(response)
        if r.get('type') == 'control_response' and r['response']['request_id'] == 'fixture-hook':
            decision = r['response'].get('response',{}).get('hookSpecificOutput',{}).get('permissionDecision', 'allow')
            with open('decision', 'w') as f: f.write(decision or 'missing')
            break
    emit({'type':'user','message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'fixture-call','content':decision,'is_error':decision != 'allow'}]}})
    emit({'type':'assistant','message':{'id':'fixture-finish','model':'haiku','content':[{'type':'text','text':'Done.'}], 'usage':{'input_tokens':1,'output_tokens':1}}})
    emit({'type':'result','subtype':'success','is_error':False,'result':'Done.','session_id':'fixture-session'})
"#).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(home.path().join("config.toml"), format!(
            "default_model = \"fixture/haiku\"\n[security]\nsandbox = \"off\"\n[lsp]\nenabled = false\n[agent]\nreview_interval = 0\nclaude_keep_open_minutes = 0\nverify_check = \"off\"\n[[provider]]\nid = \"fixture\"\nkind = \"claude_code\"\nbase_url = {:?}\n[[provider.models]]\nid = \"haiku\"\n", cli.display().to_string()
        )).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let engine = runtime
            .block_on(Engine::open(
                Paths::at(home.path()),
                EngineOptions::default(),
            ))
            .unwrap();
        runtime
            .block_on(engine.handle(
                Request::ClaudeCodeStatus {
                    refresh: true,
                    provider: Some("fixture".into()),
                },
                None,
            ))
            .unwrap();
        let parent = engine
            .0
            .create_session(
                Some(work.path().display().to_string()),
                None,
                Some("Parent".into()),
                SessionKind::Chat,
                None,
                Some(PermissionMode::Auto),
                None,
            )
            .unwrap();
        let child = engine.0.start_lock_order_test_child(&parent);
        for (mode, expected) in [
            (PermissionMode::Auto, "allow"),
            (PermissionMode::ReadOnly, "deny"),
        ] {
            let mut pst = parent.st.lock();
            pst.info.permission = mode;
            engine.0.publish_permission(&pst);
            let result =
                runtime.block_on(engine.0.claude_code_turn(&child, &CancellationToken::new()));
            assert!(
                matches!(result, Outcome::Done),
                "CLI fixture did not finish"
            );
            assert_eq!(
                std::fs::read_to_string(work.path().join("decision")).unwrap(),
                expected
            );
            drop(pst);
            engine
                .0
                .append(
                    &child,
                    EntryBody::User {
                        content: vec![ContentPart::text("Try fixture again")],
                        author: Some("harness".into()),
                    },
                )
                .unwrap();
        }
    }
}
