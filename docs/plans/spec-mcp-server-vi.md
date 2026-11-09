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
