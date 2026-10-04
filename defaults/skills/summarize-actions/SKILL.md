---
name: summarize-actions
description: Summarize supplied text and extract decisions and action items. Use for meeting notes, messages or long documents.
license: Apache-2.0
metadata:
  author: Vox
  vox.title: Summarize and extract actions
---

1. Identify the material the user supplied and the requested level of detail.
2. Summarize the central points using only that material. Attribute disputed claims and distinguish decisions from suggestions.
3. Extract action items with an owner and deadline only when the source supplies them. Mark missing owners or dates as unspecified.
4. Return a short summary followed by decisions and actions. Preserve uncertainties and source references when available.
Completion: every action is supported by the supplied material; no action has been executed.
