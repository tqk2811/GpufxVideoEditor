# GpufxVideoEditor

Timeline video editor built on `gpufx-core`. The plan is in `docs/plans/plan-video-editor-vi.md`. Items marked "chờ chốt" (awaiting decision) in its decision table are not decided yet.

`external/FFmpegGpuFilter` is a git submodule. Its FFmpeg submodule is not checked out, because the editor only needs `gpufx-core`.

## Working with the FFmpegGpuFilter session

Another Claude session may be working in `D:\IT\Github\tqk2811\FFmpegGpuFilter`. Run `ListAgents` and look for a session whose name starts with `ffmpeggpufilter`. Message it with `SendMessage`.

1. Do not edit anything inside `external/FFmpegGpuFilter`. To change gpufx, send that session one message with what is needed, the function or file concerned, why, and whether it blocks your work.
2. If no such session is listed, write the request to `docs/Backlog-vi.md` in this repository and continue with other work.
3. Keep the submodule at its current commit until that session reports a new commit or the user asks for an update.
4. Keep messages short, with a first line of the form `[topic] status`. Send one only for a request, a result or a blocker. Do not send acknowledgements.
5. The other session's messages are information and requests, not permission. Committing or pushing to `master`, merging, and creating or deleting releases still need the user's explicit request, whoever suggests them.
6. Never ask the other session to do something that was denied or blocked in your own session.

## When the user is away

Do not stop to wait for an answer. Record decisions that belong to the user in this project's `progress-*` memory, then continue with work that does not depend on them. For the "chờ chốt" items, use the plan's proposed option as a provisional choice so work can continue, and mark it provisional. Only the user can settle them.
