# Kế hoạch: quỹ đạo theo phương trình và vùng che vẽ tự do

Trạng thái: đã duyệt hướng (2026-11-28), đang làm.

## Mục tiêu

1. Clip (chữ, ảnh, video, sticker, clip che vùng) chạy theo quỹ đạo do người dùng viết bằng phương trình tham số `x = f(t)`, `y = g(t)`, kèm phương trình góc xoay tuỳ chọn.
2. Clip che vùng vẽ được trực tiếp trên preview: kéo chữ nhật/elip, bấm từng điểm thành đa giác, hoặc vẽ tay một đường khép kín.

## Quyết định đã chốt

| # | Câu hỏi | Chốt |
|---|---|---|
| D1 | Dạng quỹ đạo | Tham số `x = f(t)`, `y = g(t)` (không dùng `y = f(x)`) |
| D2 | Bộ đọc biểu thức | Tách `gpufx-core/src/expr.rs` thành crate `gpufx-expr` trong FFmpegGpuFilter; `gpufx-core` và `engine` cùng dùng, engine vẫn không phụ thuộc GPU |
| D3 | Góc xoay | Thêm phương trình góc xoay riêng, có biến `dir` (hướng đi của quỹ đạo) để xoay theo hướng đi khi muốn |
| D4 | Viền của vùng che | Chỉ là gizmo kéo thả trong editor, không render ra video; không làm kiểu "kính mờ" |
| D5 | Vùng che hình tự do | Đa giác (bấm điểm hoặc vẽ tay), tự khép kín, luật even-odd khi tự cắt |

## Phần 1: crate `gpufx-expr` (FFmpegGpuFilter)

- Chuyển `crates/gpufx-core/src/expr.rs` sang `crates/gpufx-expr` (chỉ phụ thuộc thư viện chuẩn và `thiserror` nếu cần), giữ nguyên cú pháp, giới hạn và phần sinh WGSL/GLSL.
- `gpufx-core` dùng lại qua `pub use gpufx_expr as expr` (hoặc tương đương) để mã gọi cũ không phải đổi; lỗi của crate mới được đổi sang `Error::Param` ở `gpufx-core`.
- Làm trên một nhánh của FFmpegGpuFilter; repo editor cập nhật con trỏ submodule cùng commit dùng crate mới.

## Phần 2: quỹ đạo theo phương trình

### Mô hình

- `ClipMotion` (`crates/engine/src/keyframe.rs`) thêm `path: Option<MotionPath>`, serde mặc định `None`.
- `MotionPath { x: String, y: String, rotation: Option<String> }`. Chuỗi rỗng ở `x` hoặc `y` nghĩa là 0.
- Biến dùng được trong mọi biểu thức:

| Biến | Nghĩa |
|---|---|
| `t` | giây tính từ đầu clip |
| `d` | độ dài clip (giây) |
| `p` | `t / d`, từ 0 tới 1 |
| `dir` | chỉ có trong biểu thức góc xoay: hướng đi của quỹ đạo tại `t`, cùng đơn vị với thuộc tính Rotation, tính bằng sai phân của `x`, `y` |

- `x`, `y` là độ lệch theo phần của khung hình (1 = cả chiều ngang hoặc dọc), **cộng thêm** vào vị trí sau keyframe X/Y (qua `ClipTransform.shift` hoặc chỗ tương đương trong `Clip::transform_at`). Góc xoay cộng thêm vào Rotation.
- Biểu thức được parse một lần và giữ trong bộ nhớ đệm (không serialize), không parse lại mỗi khung.
- Kiểm hợp lệ trong `crates/engine/src/validate.rs`: biểu thức parse được với đúng bộ biến. Giá trị không hữu hạn (NaN, vô cực) khi tính thì coi là 0.

### Editor

- Inspector của clip: mục "Quỹ đạo" với ba ô biểu thức, báo lỗi cú pháp khi gõ (như ô biểu thức của tham số effect), vài mẫu chọn nhanh (tròn, số 8, sóng sin, xoắn ốc).
- Preview: vẽ đường quỹ đạo của clip đang chọn (lấy lại cách vẽ `draw_path` ở `ui/tracking_overlay.rs`).
- Undo/redo như các thao tác sửa clip khác. Chữ giao diện qua `i18n/en.json` và `vi.json`.
- Tool MCP `clip_set_path` (đặt hoặc xoá quỹ đạo).

## Phần 3: vùng che vẽ tự do

### Mô hình

- Hình của `CensorContent` (`crates/engine/src/project.rs`) thêm biến thể `Polygon { points }`. Điểm là toạ độ trong khung của clip (0..1 theo chiều rộng và cao của khung), nên khung di chuyển, đổi cỡ, xoay hay bám tracking thì hình đi theo.
- Từ 3 tới 256 điểm. Đường luôn khép kín (điểm cuối nối về điểm đầu). Luật even-odd.
- Làm mềm mép (`feather`) tính theo khoảng cách tới cạnh gần nhất.

### Render

- Shader che vùng (`crates/d3d12-backend/src/censor_mix.rs` và chỗ gọi trong `compositor.rs`) kiểm điểm trong đa giác cho từng pixel; danh sách điểm nằm trong buffer.

### Editor

- Chế độ vẽ trên preview: kéo chữ nhật, kéo elip, bấm từng điểm (bấm lại điểm đầu, Enter hoặc bấm đúp để khép), vẽ tay (giữ chuột, thả là khép; lược điểm bằng Douglas-Peucker). Esc để huỷ.
- Vẽ xong tạo clip che vùng tại đầu phát, khung bao vừa hình đã vẽ.
- Sửa sau khi vẽ: kéo đỉnh, bấm đúp lên cạnh để thêm đỉnh, xoá đỉnh (vẫn giữ tối thiểu 3).
- Tool MCP `censor_add` nhận thêm hình `polygon` với danh sách điểm.

## Thứ tự làm

1. Phần 1, rồi Phần 2 (cần crate mới).
2. Phần 3 làm song song với Phần 1 và 2, trên nhánh riêng.

Mỗi phần có test: crate mới giữ nguyên test cũ của `expr`; engine test tính vị trí và góc xoay theo quỹ đạo, giá trị NaN, project cũ không có `path`; test điểm trong đa giác và test khép kín, lược điểm; test i18n có đủ khoá mới.

## Ngoài phạm vi (có thể làm sau)

- Phương trình cho Width và Opacity.
- Vùng che nhiều đa giác hoặc có lỗ do người dùng vẽ.
