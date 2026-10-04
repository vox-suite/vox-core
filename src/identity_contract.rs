/**
* Minimal caller-identity boundary type. The host application (e.g. vox-core)
* resolves its own rich identity/auth context and converts it to a
* `RequestContext` at the call boundary; this crate never resolves identity
* itself.
*/
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UserId(pub Uuid);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UserContextId(pub Uuid);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DeploymentId(pub Uuid);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestSubject {
    pub deployment_id: DeploymentId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestContext {
    pub id: UserContextId,
    pub user_id: UserId,
    pub subject: RequestSubject,
}

/// Minimal identity supplied by any host using connector services.
pub trait RequestScope {
    fn request_context(&self) -> RequestContext;
}

impl RequestScope for RequestContext {
    fn request_context(&self) -> RequestContext {
        *self
    }
}
