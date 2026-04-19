use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tv_bot_core_types::ActionSource;

use crate::{
    ControlApiCommand, ControlApiCommandResult, ControlApiCommandStatus, ControlApiEvent,
    ControlApiEventPublisher, LocalControlApi, RuntimeCommandDispatcher,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpStatusCode {
    Ok = 200,
    Conflict = 409,
    Forbidden = 403,
    PreconditionRequired = 428,
    InternalServerError = 500,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpCommandRequest {
    pub command: ControlApiCommand,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HttpResponseBody {
    CommandResult(ControlApiCommandResult),
    Error { message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpCommandResponse {
    pub status_code: HttpStatusCode,
    pub body: HttpResponseBody,
}

pub struct HttpCommandHandler<D, P = crate::NoopEventPublisher> {
    api: LocalControlApi<D>,
    publisher: P,
}

impl<D> HttpCommandHandler<D, crate::NoopEventPublisher> {
    pub fn new(api: LocalControlApi<D>) -> Self {
        Self {
            api,
            publisher: crate::NoopEventPublisher,
        }
    }
}

impl<D, P> HttpCommandHandler<D, P> {
    pub fn with_publisher(api: LocalControlApi<D>, publisher: P) -> Self {
        Self { api, publisher }
    }

    pub fn dispatcher(&self) -> &D {
        self.api.dispatcher()
    }

    pub fn dispatcher_mut(&mut self) -> &mut D {
        self.api.dispatcher_mut()
    }

    pub fn into_inner(self) -> (LocalControlApi<D>, P) {
        (self.api, self.publisher)
    }
}

impl<D, P> HttpCommandHandler<D, P>
where
    D: RuntimeCommandDispatcher,
    P: ControlApiEventPublisher,
{
    pub async fn handle_command(
        &mut self,
        request: HttpCommandRequest,
    ) -> Result<HttpCommandResponse, HttpCommandHandlerError> {
        let source = command_source(&request.command);

        match self.api.handle_command(request.command).await {
            Ok(result) => {
                self.publisher
                    .publish(ControlApiEvent::CommandResult {
                        source,
                        result: result.clone(),
                        occurred_at: Utc::now(),
                    })
                    .map_err(|source| HttpCommandHandlerError::Publish { source })?;

                Ok(HttpCommandResponse {
                    status_code: map_status_code(&result),
                    body: HttpResponseBody::CommandResult(result),
                })
            }
            Err(source) => Ok(HttpCommandResponse {
                status_code: source.status_code(),
                body: HttpResponseBody::Error {
                    message: source.to_string(),
                },
            }),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HttpCommandHandlerError {
    #[error("failed to publish websocket event: {source}")]
    Publish {
        source: crate::WebSocketEventHubError,
    },
}

fn map_status_code(result: &ControlApiCommandResult) -> HttpStatusCode {
    match result.status {
        ControlApiCommandStatus::Executed => HttpStatusCode::Ok,
        ControlApiCommandStatus::Rejected => HttpStatusCode::Conflict,
        ControlApiCommandStatus::RequiresOverride => HttpStatusCode::PreconditionRequired,
    }
}

fn command_source(command: &ControlApiCommand) -> ActionSource {
    match command {
        ControlApiCommand::ManualIntent { source, .. } => match source {
            crate::ManualCommandSource::Dashboard => ActionSource::Dashboard,
            crate::ManualCommandSource::Cli => ActionSource::Cli,
        },
        ControlApiCommand::StrategyIntent { .. } => ActionSource::System,
    }
}
