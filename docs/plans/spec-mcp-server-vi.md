# Đặc tả MCP server cho agent

Ngày lập: 2026-11-09. Trạng thái: **đề xuất, chưa code**. Kế hoạch tổng ở [plan-video-editor-vi.md](plan-video-editor-vi.md), giao diện ở [spec-ui-phase3-vi.md](spec-ui-phase3-vi.md).

Mục tiêu: agent dựng video qua MCP với cùng các lệnh mà giao diện dùng, tự xem kết quả bằng ảnh chụp, và xuất file.

## Quyết định

| Vấn đề | Lựa chọn | Trạng thái |
|---|---|---|
| Chế độ chạy | Cả hai: trên project file không cần mở app, và điều khiển cửa sổ editor đang mở | Đã chốt |
| Chụp hình | Có, theo thời điểm hoặc frame index | Đã chốt |
| Crate | `gpufx-mcp` mới, một binary, giao tiếp MCP qua stdio | Đề xuất |
| Thư viện MCP | `rmcp` (SDK Rust chính thức), kiểm phiên bản khi làm | Đề xuất |
| Nối vào app đang mở | Named pipe cục bộ của user hiện tại, app bật bằng một tuỳ chọn, mặc định tắt | Đề xuất |
| Chụp khung gốc của file chưa đưa vào timeline | Chưa làm, thêm sau nếu user cần | Đề xuất |

## Kiến trúc

Một bộ tool dùng chung cho cả hai chế độ. Tool chỉ gọi một trait `EditorSession`, nên agent không phải biết đang ở chế độ nào.

| Thành phần | Trách nhiệm |
|---|---|
| Lớp tool | Khai báo tool, kiểm tham số, đổi lỗi thành kết quả có cấu trúc |
| `EditorSession` | Đọc project, chạy lệnh sửa, undo, chụp hình, xuất file |
| `FileSession` | Giữ `Project` và `EditHistory` trong tiến trình MCP, tự mở `Gpu` khi cần chụp hoặc xuất |
| `LiveSession` | Gửi lệnh qua named pipe tới app đang mở |
| Cổng lệnh trong `gpufx-editor` | Nhận lệnh từ pipe, chạy trên luồng UI qua event loop, trả kết quả |

Lệnh sửa là các hàm thuần đã có trong `engine::edit`, xuất file dùng `ExportJob`. Chế độ file không dựng cửa sổ và không mở GPU cho tới tool đầu tiên cần tới nó.

Ở chế độ live, lệnh đi cùng đường với thao tác tay qua `EditorState`, nên preview, audio và undo tự cập nhật, và user thấy thay đổi ngay.

## Chống sửa nhầm clip

`ClipKey` là chỉ số và đổi sau khi sửa. Ở chế độ live, user có thể sửa xen giữa hai lần gọi của agent.

- Mọi kết quả trả về kèm `revision`.
- Tool sửa bắt buộc nhận `expected_revision`. Lệch thì trả lỗi `stale_revision` kèm revision mới, không sửa gì.
- Tool thêm hoặc tách clip trả về key của clip mới, tránh việc agent phải đoán lại chỉ số.

## Tool

Thời gian luôn là số nguyên tick 1/90000 giây, giống project JSON.

| Tool | Việc |
|---|---|
| `session_status` | Chế độ, đường dẫn project, revision, có đang xuất hoặc đang kéo không |
| `project_new`, `project_open`, `project_save` | Tạo, mở, lưu project |
| `project_get` | Trả project JSON, thời lượng và số khung |
| `packages_list` | Danh sách effect và transition, lọc theo chuỗi |
| `track_add` | Thêm track |
| `track_set_locked`, `track_set_hidden` | Khoá hoặc ẩn track. Sửa track đang khoá bị từ chối với lỗi `track_locked` |
| `clip_add`, `clip_move`, `clip_trim_start`, `clip_trim_end`, `clip_split`, `clip_delete` | Sửa clip |
| `effect_add`, `effect_remove`, `transition_add`, `transition_remove` | Hiệu ứng và transition |
| `clip_set_gain`, `clip_set_muted`, `clip_set_fades` | Âm lượng và fade |
| `undo`, `redo` | Hoàn tác, mỗi tool sửa là một bước |
| `frame_capture` | Chụp khung đã ghép |
| `export_start`, `export_status`, `export_cancel` | Xuất nền |

### `frame_capture`

Nhận đúng một trong hai tham số `time` (tick) hoặc `frame` (index), và `max_width` tuỳ chọn. Có cả hai hoặc thiếu cả hai là lỗi tham số. Vị trí ngoài timeline là lỗi, không chụp khung gần nhất.

Trả về ảnh PNG dạng nội dung image của MCP, cùng frame index, thời điểm thực của khung và revision. Khung đã gồm hiệu ứng và transition. `time` được đổi sang frame index bằng đúng công thức preview dùng. Ảnh được thu nhỏ khi quá `max_width`, mặc định 1280 để không làm phình ngữ cảnh agent.

Chế độ file dựng khung qua `FrameRenderer` trên device riêng. Chế độ live gửi yêu cầu cho luồng preview với một kênh trả kết quả riêng, không di chuyển đầu phát và không thay khung đang hiển thị.

### Xuất file

`export_start` trả ngay một mã công việc, `export_status` trả tiến độ, `export_cancel` huỷ. Ghi đè file có sẵn phải có `overwrite: true`. Mỗi lúc chỉ một lần xuất. Ở chế độ live, preview tạm dừng và không sửa được trong lúc xuất, giống thao tác từ giao diện.

`export_start`, `export_status` và `export_cancel` chạy `gpufx-render --progress json` thành tiến trình con, giống hệt cách editor đang làm, và dùng lại giao thức dưới đây.

### Giao thức tiến độ

`gpufx-render <project.json> --out <file> --progress json` ghi mỗi sự kiện thành một dòng JSON trên stdout. Mặc định (`--progress text`) vẫn in văn bản cho người đọc như cũ. Kiểu dữ liệu nằm ở `crates/d3d12-backend/src/progress.rs`.

| `event` | Trường khác | Ý nghĩa |
|---|---|---|
| `started` | `protocol` (hiện là 1), `adapter`, `total`, `out` | Đã mở GPU, sắp render |
| `output_reserved` | `path` | Đã tạo file tạm này, cạnh file đích |
| `encoder` | `name` | Encoder đã mở |
| `progress` | `frame`, `total`, `elapsed_ms` | Xong `frame` khung trên `total`, sau mỗi khung |
| `finished` | `report`: `encoder`, `frames`, `audio_samples`, `clamped_samples`, `pcm_sha256` | File đã nằm ở đích |
| `cancelled` | | Đã dừng và đã xoá file tạm |
| `error` | `message` | Thất bại, kèm lý do bằng tiếng Anh |

- Mỗi lượt chạy kết thúc bằng đúng một trong `finished`, `cancelled`, `error`. Lỗi đọc tham số và đọc project cũng thành `error`. Mã thoát là 0 chỉ khi `finished`.
- `finished` và `error` của bước đổi tên được ghi ngay sau bước đó, trước khi giải phóng GPU. Tiến trình crash hoặc treo sau sự kiện kết thúc không làm đổi kết quả: bên gọi tin sự kiện kết thúc, kể cả khi mã thoát khác 0, và tự kill tiến trình nếu nó không thoát sau một khoảng chờ.
- Bên đọc phải bỏ qua dòng không phải JSON hợp lệ và `event` chưa biết, vì thư viện có thể ghi vào stdout và sự kiện có thể được bổ sung về sau. `protocol` chỉ tăng khi một sự kiện có sẵn mất hoặc đổi trường.
- Gửi dòng `cancel` vào stdin để dừng sạch. Mặc định stdin đóng không có ý nghĩa gì. Thêm cờ `--cancel-on-stdin-eof` (chỉ dùng được với `--progress json`) thì stdin đóng cũng là huỷ: server headless phải bật cờ này để tiến trình con tự dừng khi server chết, vì khi đó stdin đóng theo. Editor không dùng cờ, vì đã có job object kill-on-close.
- Một dòng không dài quá 64 KB; thông báo `error` quá dài bị cắt bớt ngay trong JSON. Nếu đã xong mọi khung mà dòng cuối không đọc được thì đó có thể là sự kiện kết thúc: bên gọi không được xoá file tạm và phải báo là không rõ kết quả.
- Tiến trình chết mà không có sự kiện kết thúc (crash, bị kill) thì bên gọi tự xoá các file đã báo qua `output_reserved`. Đã có sự kiện kết thúc thì không được xoá: `error` có thể đang giữ lại file đã render xong khi đổi tên thất bại.
- stderr chỉ để con người đọc. Bên gọi phải đọc liên tục để pipe không đầy, và chỉ giữ một đoạn cuối có giới hạn.

## Lỗi

Kết quả lỗi có `code`, `message` và dữ liệu kèm theo. `EditError` đổi sang code ổn định, ví dụ `overlap`, `clip_not_found`, `no_room`, `invalid_project`. Thêm `stale_revision`, `busy` (đang kéo hoặc đang xuất), `no_app` (chế độ live không tìm thấy app), `gpu_unavailable`. Lỗi không làm chết server. GPU hỏng thì báo lỗi rõ và phải khởi động lại phiên.

## An toàn

- Pipe chỉ cho user hiện tại truy cập, không mở cổng mạng.
- App chỉ nhận lệnh khi user bật tuỳ chọn, và hiện dấu báo đang có agent nối vào.
- Đường dẫn trong tham số được chuẩn hoá. Lưu, xuất và ghi đè không bao giờ ghi vào file nguồn đang dùng, theo kiểm tra sẵn có của `ExportJob`.
- Package là code shader nạp từ đĩa. Chế độ agent chỉ nạp từ package root, không nạp theo đường dẫn nằm trong thư mục project.

## Thứ tự làm

Mỗi bước một commit, build và test xong mới sang bước sau.

1. Crate `gpufx-mcp`, trait `EditorSession`, `FileSession`, các tool đọc và sửa, undo.
2. `frame_capture` ở chế độ file, mã hoá PNG.
3. `export_*` ở chế độ file.
4. Cổng lệnh trong `gpufx-editor`, `LiveSession`, kiểm tra revision và trạng thái bận.
5. `frame_capture` và `export_*` ở chế độ live.

## Kiểm thử và tiêu chí xong

| Tiêu chí | Cách kiểm |
|---|---|
| Tool sửa đúng | Mỗi tool cho cùng project với gọi thẳng lệnh trong `engine` |
| Revision chống sửa nhầm | Gọi với revision cũ bị từ chối và project không đổi |
| Hai chế độ giống nhau | Cùng chuỗi lệnh cho project bằng nhau trên `FileSession` và `LiveSession` |
| Chụp đúng khung | Hash khung của `frame_capture` trùng hash CLI tại cùng index; `time` và `frame` tương ứng cho cùng ảnh |
| Chụp live không làm phiền | Đầu phát và khung đang hiển thị không đổi |
| Xuất qua MCP | Hash trùng CLI, huỷ không để lại file tạm |
| Dùng thật | Một client MCP thật dựng video ngắn trên cả hai chế độ, có người xem kết quả |

## Phụ thuộc và rủi ro

- `ClipKey` theo chỉ số là điểm yếu cho agent. Nếu `expected_revision` gây thử lại quá nhiều, cần chuyển sang id clip ổn định trong `engine`.
- Chế độ file và app đang mở cùng sửa một project file sẽ ghi đè lẫn nhau. Bản đầu dùng file khoá để từ chối mở lần hai.
- Chế độ file mở GPU riêng, nên chạy cùng app tốn thêm bộ nhớ video.
- Độ dài clip đọc từ file nguồn bằng `d3d12_backend::probe_media`, tách theo luồng video và audio; giao diện đã dùng nó thay cho độ dài cố định 10 giây. Agent dùng lại API này.
