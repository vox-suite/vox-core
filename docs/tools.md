# Vox Core Agent Tools

This document details all agent tools available to the Gemini conversation agent in `vox-core`. All database-backed tools are automatically user-scoped using the authenticated caller's `UserId`, guaranteeing secure multi-tenant isolation.

---

## 1. User Profile & Persona Tools

### `get_user_info`
- **Module**: `crate::agents::tools::profile::GetUserInfo`
- **Description**: Retrieves biographical facts, location, preferences, and personality traits stored for the current user.
- **Arguments**:
  - `include_persona` *(optional boolean)*: Whether to include communication style and persona settings (default: `true`).
- **Response**:
  ```json
  {
    "user_id": "uuid",
    "facts": {
      "name": "Rahul",
      "city": "Chennai",
      "occupation": "Software Engineer"
    },
    "persona": {
      "tone": "direct",
      "verbosity": "concise",
      "proactivity": "medium",
      "technical_depth": "standard"
    },
    "version": 2
  }
  ```

### `update_user_info`
- **Module**: `crate::agents::tools::profile::UpdateUserInfo`
- **Description**: Updates or merges new user profile facts (such as name, city, dietary preferences) and persona style preferences in PostgreSQL.
- **Arguments**:
  - `name` *(optional string)*: User's full or preferred name.
  - `facts` *(optional object)*: Key-value map of biographical facts, preferences, or personal notes to merge.
  - `persona` *(optional object)*: Persona trait settings (`tone`, `verbosity`, `proactivity`, `technical_depth`).
- **Response**:
  ```json
  {
    "status": "success",
    "user_id": "uuid",
    "facts": { "name": "Rahul", "city": "Chennai" },
    "persona": { "tone": "direct", "verbosity": "concise" }
  }
  ```

---

## 2. Project Management Tools

### `create_project`
- **Module**: `crate::agents::tools::projects::CreateProject`
- **Description**: Creates a new project to group tasks, goals, and plans.
- **Arguments**:
  - `name` *(required string)*: The title or name of the project.
  - `description` *(optional string)*: Summary or objective of the project.
- **Response**:
  ```json
  {
    "status": "created",
    "project_id": "uuid",
    "name": "Vox Mac Client",
    "description": "Desktop daemon for remote actions"
  }
  ```

### `list_projects`
- **Module**: `crate::agents::tools::projects::ListProjects`
- **Description**: Lists existing projects for the caller with their current status and task count.
- **Arguments**:
  - `status` *(optional string)*: Filter by `active`, `paused`, `completed`, `archived`, or `all` (default: `active`).
- **Response**:
  ```json
  {
    "projects": [
      {
        "id": "uuid",
        "name": "Vox Mac Client",
        "description": "Desktop daemon",
        "status": "active",
        "task_count": 4
      }
    ]
  }
  ```

### `get_project`
- **Module**: `crate::agents::tools::projects::GetProject`
- **Description**: Retrieves full project details along with all associated tasks.
- **Arguments**:
  - `project_id` *(optional string)*: UUID of the project.
  - `name` *(optional string)*: Exact or case-insensitive project name.
- **Response**:
  ```json
  {
    "id": "uuid",
    "name": "Vox Mac Client",
    "description": "Desktop daemon",
    "status": "active",
    "tasks": [
      {
        "id": "uuid",
        "title": "Build WebSocket listener",
        "status": "completed",
        "execution_type": "autonomous"
      }
    ]
  }
  ```

### `update_project`
- **Module**: `crate::agents::tools::projects::UpdateProject`
- **Description**: Updates project metadata or moves project state (`active`, `paused`, `completed`, `archived`).
- **Arguments**:
  - `project_id` *(required string)*: UUID of the project.
  - `name` *(optional string)*: New project name.
  - `description` *(optional string)*: New project description.
  - `status` *(optional string)*: New status (`active`, `paused`, `completed`, `archived`).

---

## 3. Task Management & Execution Tools

### `create_task`
- **Module**: `crate::agents::tools::tasks::CreateTask`
- **Description**: Creates a new task. Automatically binds to a project if `project_name` or `project_id` is supplied (auto-creates the project if it does not yet exist).
- **Arguments**:
  - `title` *(required string)*: Short title or summary of the task.
  - `instruction` *(optional string)*: Detailed instructions or steps needed to execute the task.
  - `project_name` *(optional string)*: Target project name for automatic binding.
  - `project_id` *(optional string)*: Target project UUID.
  - `execution_type` *(optional string)*: `autonomous`, `interactive`, or `manual_human` (default: `manual_human`).
  - `due_at` *(optional string)*: ISO 8601 timestamp for the task deadline.
- **Response**:
  ```json
  {
    "status": "created",
    "task_id": "uuid",
    "title": "Open terminal and run build",
    "project_id": "uuid",
    "execution_type": "autonomous"
  }
  ```

### `list_tasks`
- **Module**: `crate::agents::tools::tasks::ListTasks`
- **Description**: Lists user tasks filtered by status and project.
- **Arguments**:
  - `status` *(optional string)*: `pending`, `evaluating`, `executing`, `waiting_user`, `completed`, `failed`, `cancelled`, or `all` (default: `pending`).
  - `project_id` *(optional string)*: Filter tasks under a specific project.
  - `limit` *(optional integer)*: Maximum records to return (1–100, default: 20).

### `get_task`
- **Module**: `crate::agents::tools::tasks::GetTask`
- **Description**: Retrieves full details of a task, including raw instructions, feasibility reasoning, and execution results.
- **Arguments**:
  - `task_id` *(optional string)*: Task UUID.
  - `title_query` *(optional string)*: Substring title search.

### `update_task`
- **Module**: `crate::agents::tools::tasks::UpdateTask`
- **Description**: Updates task status or records autonomous execution results.
- **Arguments**:
  - `task_id` *(required string)*: Task UUID.
  - `status` *(optional string)*: `pending`, `evaluating`, `executing`, `waiting_user`, `completed`, `failed`, `cancelled`.
  - `feasibility_reasoning` *(optional string)*: Reason for execution classification.
  - `execution_result` *(optional object)*: Output data from the execution worker.

---

## 4. Personal Records & Goal Tracking Tools

### `create_user_record`
- **Module**: `crate::agents::tools::records::CreateUserRecord`
- **Description**: Stores structured personal data across multiple domains.
- **Arguments**:
  - `domain` *(required string)*: `finance`, `health`, `work`, `knowledge`, `wishlist`, `hobbies`, `general`.
  - `entity_type` *(required string)*: Entity type, e.g. `transaction`, `sleep`, `note`, `item`, `commit`.
  - `title` *(required string)*: Headline or label for the entry.
  - `data` *(optional object)*: Structured payload (e.g. `{ "amount": 45.0, "currency": "INR", "category": "dining" }`).

### `list_user_records`
- **Module**: `crate::agents::tools::records::ListUserRecords`
- **Description**: Queries personal history and logged data entries by domain and entity type.
- **Arguments**:
  - `domain` *(optional string)*: Domain filter.
  - `entity_type` *(optional string)*: Specific entity type.
  - `limit` *(optional integer)*: Maximum results (default: 15).

### `manage_user_goal`
- **Module**: `crate::agents::tools::records::ManageUserGoal`
- **Description**: Sets or updates targets and budgets (e.g., monthly spending limits, weekly habit milestones).
- **Arguments**:
  - `domain` *(required string)*: `finance`, `health`, `work`, `habit`.
  - `title` *(required string)*: Goal name.
  - `description` *(optional string)*: Goal details.
  - `target_metric` *(optional object)*: Metric configuration (e.g. `{ "limit_amount": 5000, "period": "monthly" }`).
  - `status` *(optional string)*: `active`, `paused`, `completed`, `abandoned`.

---

## 5. Client Device & Execution Dispatch Tools

### `list_devices`
- **Module**: `crate::agents::tools::devices::ListDevices`
- **Description**: Lists active desktop and mobile clients linked to the user account with live telemetry (battery, active window, location).
- **Arguments**: None.
- **Response**:
  ```json
  {
    "devices": [
      {
        "id": "uuid",
        "identifier": "macbook-pro",
        "platform": "darwin",
        "name": "Rahul's MacBook Pro",
        "last_seen_at": "2026-09-16T15:20:00Z",
        "telemetry": { "active_app": "Terminal", "battery": 88 }
      }
    ]
  }
  ```

### `dispatch_device_command`
- **Module**: `crate::agents::tools::devices::DispatchDeviceCommand`
- **Description**: Dispatches a remote command into the `actions` queue to execute on a connected client machine (e.g., opening Terminal on macOS).
- **Arguments**:
  - `target_device` *(optional string)*: Platform or device name filter (`darwin`, `mac`, `phone`).
  - `command_type` *(required string)*: `launch_app`, `run_terminal`, `open_url`, `system_control`.
  - `command_payload` *(optional object)*: Parameters (e.g. `{ "app_name": "Terminal" }`).
- **Response**:
  ```json
  {
    "status": "queued",
    "action_id": "uuid",
    "target_device": "Rahul's MacBook Pro",
    "command": "launch_app"
  }
  ```

---

## 6. External Web & Spatial Tools

### `web_search`
- **Module**: `crate::agents::tools::web_search::WebSearch`
- **Description**: Searches the live web using Exa for real-time information.
- **Arguments**: `query` (string).

### `search_places`
- **Module**: `crate::agents::tools::google_maps::SearchPlaces`
- **Description**: Finds real-world places and businesses using Google Places.
- **Arguments**: `query` (string), optional `latitude`, `longitude`, `radius_meters`.

### `get_route`
- **Module**: `crate::agents::tools::google_maps::GetRoute`
- **Description**: Computes route distance and travel time between two waypoints.
- **Arguments**: `origin` (string), `destination` (string), optional `travel_mode`.

---

## 7. Telephony & Outbound Calling Tools

### `schedule_outbound_call`
- **Module**: `crate::agents::tools::calls::ScheduleOutboundCall`
- **Description**: Schedules a reminder or notification that automatically triggers an outbound phone call to the user when due. Vox schedules the task, and at the designated time, initiates a phone call to the user via Twilio. When answered, Vox immediately speaks the opening instruction before continuing the conversational turn.
- **Example user prompts**:
  - *"Call me after 5 min and remind me to clean my room"*
  - *"Call me in 30 minutes to check if the oven is preheated"*
  - *"Schedule a reminder call for tomorrow at 9 AM to review the project roadmap"*
- **Arguments**:
  - `reason` *(required string)*: Short title or description of the reminder or call reason (e.g. `"Clean room reminder"`).
  - `opening_instruction` *(required string)*: Spoken prompt / greeting that Vox should speak as soon as the user answers the phone call (e.g. `"Remind the user to clean their room as requested 5 minutes ago"`).
  - `delay_seconds` *(optional integer)*: Number of seconds from now to place the call (e.g. `300` for 5 minutes).
  - `delay_minutes` *(optional integer)*: Number of minutes from now to place the call (e.g. `5`).
  - `run_at` *(optional string)*: Exact ISO 8601 timestamp for when to place the call (e.g. `"2026-09-21T15:30:00Z"`).
  - `phone_number` *(optional string)*: Phone number override in E.164 format (e.g. `"+1234567890"`). If omitted, Vox uses the user's registered phone identity.
- **Response**:
  ```json
  {
    "status": "scheduled",
    "schedule_id": "019207e0-4702-7c38-89c0-112233445566",
    "task_id": "019207e0-4702-7c38-89c0-998877665544",
    "phone_number": "+1234567890",
    "scheduled_time": "2026-09-21T15:35:00Z",
    "reason": "Clean room reminder",
    "opening_instruction": "Remind the user to clean their room as requested 5 minutes ago",
    "message": "Outbound call scheduled for 2026-09-21T15:35:00Z to call +1234567890 with reminder: Clean room reminder"
  }
  ```

### `trigger_outbound_call`
- **Module**: `crate::agents::tools::calls::TriggerOutboundCall`
- **Description**: Immediately initiates an outbound phone call to the user to speak with them live right now (e.g., for critical alerts or when requested immediately).
- **Arguments**:
  - `reason` *(required string)*: Brief internal description of why the call is being made.
  - `opening_instruction` *(required string)*: Spoken instruction Vox will deliver immediately upon the user answering.
  - `phone_number` *(optional string)*: Phone number override in E.164 format. If omitted, uses the user's registered phone identity.
- **Response**:
  ```json
  {
    "status": "call_initiated",
    "call_id": "019207e0-4702-7c38-89c0-112233445566",
    "conversation_id": "019207e0-4702-7c38-89c0-776655443322",
    "phone_number": "+1234567890",
    "provider_call_id": "CA1234567890abcdef",
    "reason": "Critical server alert",
    "opening_instruction": "Inform the user that the production cluster reported high CPU usage",
    "message": "Outbound call initiated to +1234567890 for Critical server alert"
  }
  ```
