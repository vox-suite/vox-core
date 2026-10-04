# Vox Device Protocol Specification

This document defines the contract between Vox Core and connected client devices (Desktop, Mobile, etc.) for registration, heartbeats, and local compute/inference job execution.

## 1. Device Enrollment & Identity

Devices must register with Vox Core using a cryptographic keypair or secure enrollment token.

### Registration
* **Endpoint:** `POST /v1/devices`
* **Request:**
  ```json
  {
    "device_identifier": "desktop-client-uuid",
    "platform": "macos-aarch64",
    "label": "Rahul's M3 MacBook Pro",
    "public_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI...",
    "capabilities": {
      "local_models": ["gemini-nano", "qwen-2.5-7b"],
      "max_context_window": 32768,
      "supports_embedding": true
    },
    "execution_consent": true
  }
  ```
* **Response (201 Created):**
  ```json
  {
    "id": "device-uuid",
    "device_identifier": "desktop-client-uuid",
    "is_active": true,
    "last_seen_at": "2026-09-23T01:00:00Z"
  }
  ```

## 2. Heartbeat & Presence

Devices must periodically heartbeat to indicate availability and report resource telemetry.

* **Endpoint:** `POST /v1/devices/{id}/heartbeat`
* **Cadence:** Recommended every 30 seconds.
* **Request:**
  ```json
  {
    "telemetry": {
      "battery_level": 0.85,
      "is_charging": true,
      "thermal_state": "nominal",
      "available_memory_mb": 16384
    }
  }
  ```
* **Response (200 OK):**
  ```json
  {
    "status": "active",
    "assigned_jobs_count": 0
  }
  ```

## 3. Work Dispatch

Work reaches a device over its socket (`GET /v1/devices/{id}/socket`, WebSocket upgrade). Core dispatches through `POST /internal/v1/devices/dispatch`. There is no polling or claim endpoint.

## 4. Security Invariants
- Devices never receive database connection strings or master secret keys.
- Devices never perform direct connector side effects (e.g. initiating real payments, modifying third-party bank accounts).
- All device outputs are validated against the server-side JSON schema before commitment to `records` or `tasks`.
- If a device lease expires, Vox Core recovers the job for server-side cloud fallback; late device submissions are safely rejected.

## 5. User Event Socket & Map Scenes

User devices maintain a live event stream via `GET /v1/me/events/socket?platform={platform}` (`Sec-WebSocket-Protocol: vox.v1`). Core pushes real-time notifications and `map_scene` frames (`{"type":"map_scene","scene":MapScene}`). Clients query initial state with `GET /v1/me/map/scene`.
