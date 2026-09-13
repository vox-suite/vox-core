#[derive(Clone)]
pub struct ToolDependencies {
    pub http: reqwest::Client,
}

impl ToolDependencies {
    pub fn new() -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("vox-bridge/1.0")
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self { http })
    }
}
