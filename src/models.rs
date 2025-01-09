use serde::{Serialize, Deserialize};

#[derive(Serialize, Debug)]
#[serde(tag = "type")]
pub enum OverwatchMsg {
    #[serde(rename = "heartbeat")]
    Heartbeat {},
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum AdvisorMsg {
    #[serde(rename = "heartbeat")]
    Heartbeat { incognito: bool },
    #[serde(rename = "navigation")]
    Navigation { url: String },
}

