/**
* Shared agent tool dependency container providing database and API access.
*/
#[derive(Clone)]
pub struct ToolDependencies {
    pub http: reqwest::Client,
}

impl ToolDependencies {
    pub fn new() -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .tcp_nodelay(true)
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .user_agent("vox-bridge/1.0")
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self { http })
    }
}
