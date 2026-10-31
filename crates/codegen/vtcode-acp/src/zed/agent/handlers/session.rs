//! ACP session and cancellation request handlers.

use super::ZedAgent;
use crate::acp::Error as SdkError;
use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, CloseSessionResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, ResumeSessionRequest,
    ResumeSessionResponse, SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
};
use agent_client_protocol::{
    Agent, Builder, Client, ConnectionTo, HandleDispatchFrom, Responder, RunWithConnectionTo, on_receive_request,
};
use std::sync::Arc;
use tracing::warn;

pub(super) fn register_session_handlers<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    let new_session_agent = Arc::clone(&agent);
    let load_session_agent = Arc::clone(&agent);
    let list_sessions_agent = Arc::clone(&agent);
    let resume_session_agent = Arc::clone(&agent);
    let close_session_agent = Arc::clone(&agent);
    builder
        .on_receive_request(
            async move |req: NewSessionRequest,
                        request_cx: Responder<NewSessionResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_new_session(Arc::clone(&new_session_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: LoadSessionRequest,
                        request_cx: Responder<LoadSessionResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_load_session(Arc::clone(&load_session_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ListSessionsRequest,
                        request_cx: Responder<ListSessionsResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_list_sessions(Arc::clone(&list_sessions_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ResumeSessionRequest,
                        request_cx: Responder<ResumeSessionResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_resume_session(Arc::clone(&resume_session_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: CloseSessionRequest,
                        request_cx: Responder<CloseSessionResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_close_session(Arc::clone(&close_session_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
}

pub(super) fn register_session_config_handler<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    builder.on_receive_request(
        async move |req: SetSessionConfigOptionRequest,
                    request_cx: Responder<SetSessionConfigOptionResponse>,
                    _cx: ConnectionTo<Client>| {
            handle_set_session_config_option(Arc::clone(&agent), req, request_cx).await
        },
        on_receive_request!(),
    )
}

async fn handle_new_session(
    agent: Arc<ZedAgent>,
    req: NewSessionRequest,
    request_cx: Responder<NewSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.new_session(req).await?;
    request_cx.respond(response)
}

async fn handle_load_session(
    agent: Arc<ZedAgent>,
    req: LoadSessionRequest,
    request_cx: Responder<LoadSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.load_session(req).await?;
    request_cx.respond(response)
}

async fn handle_list_sessions(
    agent: Arc<ZedAgent>,
    req: ListSessionsRequest,
    request_cx: Responder<ListSessionsResponse>,
) -> Result<(), SdkError> {
    let response = agent.list_sessions(req).await?;
    request_cx.respond(response)
}

async fn handle_resume_session(
    agent: Arc<ZedAgent>,
    req: ResumeSessionRequest,
    request_cx: Responder<ResumeSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.resume_session(req).await?;
    request_cx.respond(response)
}

async fn handle_close_session(
    agent: Arc<ZedAgent>,
    req: CloseSessionRequest,
    request_cx: Responder<CloseSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.close_session(&req.session_id).await?;
    request_cx.respond(response)
}

async fn handle_set_session_config_option(
    agent: Arc<ZedAgent>,
    req: SetSessionConfigOptionRequest,
    request_cx: Responder<SetSessionConfigOptionResponse>,
) -> Result<(), SdkError> {
    let response = agent.set_session_config_option(req).await?;
    request_cx.respond(response)
}

pub(super) async fn handle_cancel(agent: Arc<ZedAgent>, notif: CancelNotification) -> Result<(), SdkError> {
    match agent.session_handle(&notif.session_id) {
        Ok(Some(session)) => session.cancellation.cancel(),
        Ok(None) => {}
        Err(error) => warn!(%error, session_id = %notif.session_id, "Failed to look up ACP session for cancellation"),
    }
    Ok(())
}
