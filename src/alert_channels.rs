//! Where alerts are sent: desktop notifications and ntfy push messages.
//!
//! The app's chart alerts, the alert engine (in the app or in
//! `orbit-tape watch`) and the test button all go through here. A new
//! channel is a new field in `data::config::alert_store::Channels`, a new
//! `Channel` choice and one more function next to [`ntfy`].

use data::config::alert_store::{Channel, NtfySettings};
use std::time::Duration;

/// One alert, ready to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    /// First line, e.g. `BTCUSDT 1h: LTF MSS down`.
    pub summary: String,
    /// The rest, one fact per line.
    pub body: String,
}

/// Sends `alert` to the chosen channels without blocking. Failures are
/// logged; ntfy failures are also passed to `on_ntfy_error`.
pub fn send(
    alert: Outgoing,
    channel: Channel,
    ntfy_settings: &NtfySettings,
    on_ntfy_error: impl FnOnce(String) + Send + 'static,
) {
    if channel.desktop() {
        desktop(alert.summary.clone(), alert.body.clone());
    }
    if channel.ntfy() {
        if ntfy_settings.is_set() {
            spawn_ntfy(alert, ntfy_settings.clone(), on_ntfy_error);
        } else {
            log::warn!("alert wants ntfy, but no ntfy topic is set");
            on_ntfy_error("no ntfy topic is set (Settings, Alerts)".to_owned());
        }
    }
}

/// Shows a desktop notification on a background thread.
pub fn desktop(summary: String, body: String) {
    let spawned = std::thread::Builder::new()
        .name("alert-notification".to_owned())
        .spawn(move || {
            let result = notify_rust::Notification::new()
                .appname("Orbit Tape")
                .summary(&summary)
                .body(&body)
                .icon("appointment-soon")
                .timeout(notify_rust::Timeout::Milliseconds(15_000))
                .show();
            if let Err(err) = result {
                log::warn!("alert notification failed: {err}");
            }
        });
    if let Err(err) = spawned {
        log::warn!("alert notification thread failed: {err}");
    }
}

/// Publishes to ntfy on a background thread with its own small runtime, so
/// callers without one (the UI thread) can use it.
pub fn spawn_ntfy(
    alert: Outgoing,
    settings: NtfySettings,
    on_error: impl FnOnce(String) + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("alert-ntfy".to_owned())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| runtime.block_on(ntfy(&alert, &settings)));
            if let Err(error) = result {
                log::warn!("ntfy: {error}");
                on_error(error);
            }
        });
    if let Err(err) = spawned {
        log::warn!("ntfy thread failed: {err}");
    }
}

/// The JSON message ntfy's publish API takes (POST to the server root).
fn ntfy_message(alert: &Outgoing, settings: &NtfySettings) -> serde_json::Value {
    serde_json::json!({
        "topic": settings.topic.trim(),
        "title": alert.summary,
        "message": alert.body,
        // 4 = high: the phone makes a sound even when ntfy's default is quiet.
        "priority": 4,
        "tags": ["chart_with_upwards_trend"],
    })
}

/// Publishes one message to ntfy.
pub async fn ntfy(alert: &Outgoing, settings: &NtfySettings) -> Result<(), String> {
    let server = settings.server();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("could not build the HTTP client: {error}"))?;
    let response = client
        .post(server.as_str())
        .json(&ntfy_message(alert, settings))
        .send()
        .await
        .map_err(|error| format!("could not reach {server}: {error}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let text = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|json| json["error"].as_str().map(str::to_owned))
        .unwrap_or_else(|| text.chars().take(200).collect());
    Err(format!("{server} answered {status}: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntfy_message_carries_topic_title_and_body() {
        let settings = NtfySettings {
            topic: " orbit-alerts ".to_owned(),
            server: String::new(),
        };
        let alert = Outgoing {
            summary: "BTCUSDT 1h: LTF MSS down".to_owned(),
            body: "Roman Trade Setup\nClose 95567.3".to_owned(),
        };
        let json = ntfy_message(&alert, &settings);
        assert_eq!(json["topic"], "orbit-alerts");
        assert_eq!(json["title"], "BTCUSDT 1h: LTF MSS down");
        assert_eq!(json["message"], "Roman Trade Setup\nClose 95567.3");
        assert_eq!(settings.server(), "https://ntfy.sh");
    }
}
