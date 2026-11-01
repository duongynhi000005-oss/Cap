//! Background upload-speed health check for Instant Mode resolution capping.

use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use cap_recording::FREE_INSTANT_MODE_MAX_RESOLUTION;
use gpui::App;
use serde_json::json;

pub(crate) const HD_INSTANT_MODE_MAX_RESOLUTION: u32 = 1080;
pub(crate) const QHD_INSTANT_MODE_MAX_RESOLUTION: u32 = 1440;
pub(crate) const UHD_INSTANT_MODE_MAX_RESOLUTION: u32 = 2160;

const UPLOAD_HEALTH_PATH: &str = "/api/network/upload-health";
const PROBE_PAYLOAD_BYTES: usize = 512 * 1024;
const PROBE_RETRY_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UploadHealthSnapshot {
    pub(crate) testing: bool,
    pub(crate) measured_mbps: Option<f64>,
    pub(crate) recommended_max_resolution: Option<u32>,
    pub(crate) last_attempt_at: Option<Instant>,
}

static STATE: OnceLock<Mutex<UploadHealthSnapshot>> = OnceLock::new();

fn state_lock() -> &'static Mutex<UploadHealthSnapshot> {
    STATE.get_or_init(|| Mutex::new(UploadHealthSnapshot::default()))
}

pub(crate) fn snapshot() -> UploadHealthSnapshot {
    *state_lock().lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn is_testing() -> bool {
    snapshot().testing
}

pub(crate) fn recommended_max_resolution_for_mbps(mbps: f64) -> u32 {
    if !mbps.is_finite() || mbps <= 0.0 {
        return UHD_INSTANT_MODE_MAX_RESOLUTION;
    }
    if mbps < 5.0 {
        FREE_INSTANT_MODE_MAX_RESOLUTION
    } else if mbps < 10.0 {
        HD_INSTANT_MODE_MAX_RESOLUTION
    } else if mbps < 25.0 {
        QHD_INSTANT_MODE_MAX_RESOLUTION
    } else {
        UHD_INSTANT_MODE_MAX_RESOLUTION
    }
}

pub(crate) fn resolution_tier_label(resolution: u32) -> &'static str {
    match resolution {
        FREE_INSTANT_MODE_MAX_RESOLUTION => "720p",
        HD_INSTANT_MODE_MAX_RESOLUTION => "1080p",
        QHD_INSTANT_MODE_MAX_RESOLUTION => "1440p",
        UHD_INSTANT_MODE_MAX_RESOLUTION => "4K",
        _ => "720p",
    }
}

pub(crate) fn clamp_instant_resolution_with_probe(
    desired: u32,
    recommended_max: Option<u32>,
) -> u32 {
    match recommended_max {
        Some(cap) => desired.clamp(
            FREE_INSTANT_MODE_MAX_RESOLUTION,
            cap.max(FREE_INSTANT_MODE_MAX_RESOLUTION),
        ),
        None => desired,
    }
}

pub(crate) fn clamp_instant_resolution(desired: u32) -> u32 {
    ensure_probe_if_unmeasured();
    clamp_instant_resolution_with_probe(desired, snapshot().recommended_max_resolution)
}

pub(crate) fn active_cap_for(desired: u32) -> Option<(f64, u32)> {
    ensure_probe_if_unmeasured();
    let snap = snapshot();
    let mbps = snap.measured_mbps?;
    let cap = snap.recommended_max_resolution?;
    let clamped = clamp_instant_resolution_with_probe(desired, Some(cap));
    (clamped < desired).then_some((mbps, clamped))
}

pub(crate) fn init(cx: &mut App) {
    spawn_probe(cx);
}

pub(crate) fn retest(cx: &mut App) {
    spawn_probe(cx);
}

pub(crate) fn on_auth_changed() {
    {
        let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
        guard.measured_mbps = None;
        guard.recommended_max_resolution = None;
        guard.last_attempt_at = None;
    }
    spawn_detached_probe();
}

fn ensure_probe_if_unmeasured() {
    if !crate::store::auth_snapshot().signed_in() {
        return;
    }
    let should_spawn = {
        let guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
        !guard.testing
            && guard.measured_mbps.is_none()
            && guard
                .last_attempt_at
                .is_none_or(|t| t.elapsed() >= PROBE_RETRY_COOLDOWN)
    };
    if should_spawn {
        spawn_detached_probe();
    }
}

fn try_begin_probe() -> bool {
    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    if guard.testing {
        return false;
    }
    guard.testing = true;
    guard.last_attempt_at = Some(Instant::now());
    true
}

fn finish_probe(result: Option<f64>) {
    let mut guard = state_lock().lock().unwrap_or_else(|e| e.into_inner());
    guard.testing = false;
    guard.measured_mbps = result;
    guard.recommended_max_resolution = result.map(recommended_max_resolution_for_mbps);
}

fn spawn_detached_probe() {
    if !try_begin_probe() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("cap-upload-health-probe".into())
        .spawn(|| {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
                .and_then(|rt| rt.block_on(run_upload_probe()));
            finish_probe(result);
        });
}

fn spawn_probe(cx: &mut App) {
    if !try_begin_probe() {
        return;
    }
    crate::app_windows::refresh_settings(cx);

    cx.spawn(async move |cx| {
        let result = cx.background_executor().spawn(run_upload_probe()).await;
        finish_probe(result);
        let _ = cx.update(crate::app_windows::refresh_settings);
    })
    .detach();
}

async fn run_upload_probe() -> Option<f64> {
    if !crate::store::auth_snapshot().signed_in() {
        return None;
    }
    let client = crate::auth::api_client().ok()?;

    // Warm up DNS, TCP/TLS handshake, and auth verification on the pooled
    // connection before timing the throughput transfer.
    let warmup = crate::auth::authed_request_with(
        &client,
        reqwest::Method::POST,
        UPLOAD_HEALTH_PATH,
        Some(json!({ "payload": "0" })),
    )
    .await
    .ok()?;
    if !warmup.status().is_success() {
        return None;
    }
    let _ = warmup.bytes().await;

    let payload = "0".repeat(PROBE_PAYLOAD_BYTES);
    let body = json!({ "payload": payload });
    let start = Instant::now();
    let response = crate::auth::authed_request_with(
        &client,
        reqwest::Method::POST,
        UPLOAD_HEALTH_PATH,
        Some(body),
    )
    .await
    .ok()?;

    if !response.status().is_success() {
        return None;
    }
    let _ = response.bytes().await.ok()?;

    let elapsed = start.elapsed().as_secs_f64();
    if elapsed <= 0.0 {
        return None;
    }

    let bits = (PROBE_PAYLOAD_BYTES as f64) * 8.0;
    let mbps = bits / (elapsed * 1_000_000.0);
    tracing::debug!(
        mbps,
        recommended_max_resolution = recommended_max_resolution_for_mbps(mbps),
        "Upload speed health check completed"
    );
    Some(mbps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_upload_speed_to_expected_resolution_caps() {
        assert_eq!(
            recommended_max_resolution_for_mbps(0.0),
            UHD_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            recommended_max_resolution_for_mbps(f64::NAN),
            UHD_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            recommended_max_resolution_for_mbps(4.99),
            FREE_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            recommended_max_resolution_for_mbps(5.0),
            HD_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            recommended_max_resolution_for_mbps(9.99),
            HD_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            recommended_max_resolution_for_mbps(10.0),
            QHD_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            recommended_max_resolution_for_mbps(24.99),
            QHD_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            recommended_max_resolution_for_mbps(25.0),
            UHD_INSTANT_MODE_MAX_RESOLUTION
        );
    }

    #[test]
    fn clamps_desired_resolution_only_when_probe_recommends_lower_tier() {
        assert_eq!(
            clamp_instant_resolution_with_probe(
                UHD_INSTANT_MODE_MAX_RESOLUTION,
                Some(HD_INSTANT_MODE_MAX_RESOLUTION)
            ),
            HD_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            clamp_instant_resolution_with_probe(
                HD_INSTANT_MODE_MAX_RESOLUTION,
                Some(QHD_INSTANT_MODE_MAX_RESOLUTION)
            ),
            HD_INSTANT_MODE_MAX_RESOLUTION
        );
        assert_eq!(
            clamp_instant_resolution_with_probe(UHD_INSTANT_MODE_MAX_RESOLUTION, None),
            UHD_INSTANT_MODE_MAX_RESOLUTION
        );
    }

    #[test]
    fn failed_probe_clears_previous_cap() {
        finish_probe(Some(6.0));
        assert_eq!(
            snapshot().recommended_max_resolution,
            Some(HD_INSTANT_MODE_MAX_RESOLUTION)
        );
        finish_probe(None);
        assert_eq!(snapshot().measured_mbps, None);
        assert_eq!(snapshot().recommended_max_resolution, None);
    }
}
