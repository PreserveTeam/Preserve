use anyhow::{Context, Result, bail};
use reqwest::Client;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::models::{DeviceExchange, DownloadPlan, GamesResponse, SessionResponse};

#[derive(Clone)]
pub struct Api {
    base_url: String,
    client: Client,
}

impl Api {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let client = Client::builder()
            .user_agent(concat!("Preserve/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(20))
            .read_timeout(std::time::Duration::from_secs(120))
            .build()?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client,
        })
    }

    pub async fn games(&self) -> Result<Vec<crate::models::Game>> {
        let response = self
            .client
            .get(format!("{}/v1/games", self.base_url))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("Catalog is unavailable");
        }
        Ok(response.json::<GamesResponse>().await?.games)
    }

    pub async fn download_plan(&self, game_id: &str) -> Result<DownloadPlan> {
        let hwid = device_id()?;
        let nonce = Uuid::new_v4().simple().to_string();
        let proof = hex::encode(Sha256::digest(format!("{hwid}:{nonce}").as_bytes()));
        let session = self
            .client
            .post(format!("{}/v1/auth/device", self.base_url))
            .json(&DeviceExchange {
                hwid: &hwid,
                nonce: &nonce,
                proof: &proof,
            })
            .send()
            .await
            .context("Could not authenticate this device")?;
        if !session.status().is_success() {
            bail!("This device could not be authenticated");
        }
        let token = session.json::<SessionResponse>().await?.access_token;
        let response = self
            .client
            .get(format!("{}/v1/games/{}/download", self.base_url, game_id))
            .bearer_auth(token)
            .send()
            .await
            .context("Could not create a download plan")?;
        if !response.status().is_success() {
            bail!("This game is not available right now");
        }
        Ok(response.json::<DownloadPlan>().await?)
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
}

fn device_id() -> Result<String> {
    #[cfg(windows)]
    {
        use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};
        let key =
            RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey("SOFTWARE\\Microsoft\\Cryptography")?;
        let machine_guid: String = key.get_value("MachineGuid")?;
        Ok(hex::encode(Sha256::digest(
            format!("preserve:v1:{machine_guid}").as_bytes(),
        )))
    }
    #[cfg(not(windows))]
    {
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "preserve".into());
        Ok(hex::encode(Sha256::digest(
            format!("preserve:v1:{host}").as_bytes(),
        )))
    }
}
