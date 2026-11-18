# Kế hoạch: Object tracking (sticker, text, che vùng bám theo vật)

Mục tiêu: người dùng chọn một vật trong clip video. Sticker, text hoặc vùng che (blur, pixelate, hộp màu) gắn vào vật đó và tự di chuyển theo, có thể bám cả scale và góc xoay. Tham khảo: Tracking của CapCut, Motion tracking của DaVinci Resolve, Motion Tracker của Shotcut, tracker OpenCV trong Kdenlive.

Trạng thái: đã duyệt, các câu hỏi đã chốt ở [Câu hỏi cần chốt](#câu-hỏi-cần-chốt). P0 và P1 đã xong: tracker cổ điển ở `crates/engine/src/tracker/`, worker nền ở `crates/gpufx-editor/src/tracking.rs`, tool `track_run`, `track_status`, `track_cancel`. P2 đã xong: mục Tracking trong inspector của clip video (`ui/tracking_inspector.rs`), vẽ quỹ đạo và sửa khung trên preview (`ui/tracking_overlay.rs`), dải độ tin cậy trên timeline, thao tác ở `track_edit.rs`, tool `track_create`, `track_smooth`, `track_bake_keyframes`, `track_run` với `direction: both`. P3 đã xong trên nhánh, trừ release model: op `correlation` và activation `exp` trong `gpufx-core/nn`, `NanoTrack` (`gpufx-core/src/nn/track.rs`, cắt ảnh và giải mã khung bao bằng WGSL), script `scripts/convert-tracking-models.py` của FFmpegGpuFilter, bộ tải model chung `model_download.rs`, model tracker ở `tracking_models.rs`, chế độ Nhanh/Bền trong inspector và tham số `mode` của `track_run`. Còn chờ user tạo release `models-tracking-v1` rồi ghi `sha256_zip`, xem [Lựa chọn tạm ở P3](#lựa-chọn-tạm-ở-p3).

## Hiện trạng

- **Keyframe:** `crates/engine/src/keyframe.rs`.
  - Keyframe tính bằng tick timeline kể từ đầu clip, và giữ đúng khung nguồn khi trim, split hay đổi speed.
  - `ClipProperty` gồm X, Y, Width, Opacity, Rotation. Không có Scale riêng: Width đóng vai scale.
- **Vị trí clip:** `ClipPlacement` (`project.rs`) gồm x, y của góc trái trên và width, tính theo phần của khung hình.
- **Speed, phát ngược, `source_in`:** ánh xạ thời gian nguồn đã có sẵn (`source_time_at`, `plays_reversed`).
- **Gizmo trên preview:** `crates/gpufx-editor/src/ui/placement.rs` kéo và đổi kích thước được, dùng lại được để vẽ khung chọn vật.
- **Effect che vùng có sẵn:** `pixelate`, `gaussian-blur`, `box-blur`, `draw-box`… Tất cả áp lên cả clip, chưa có tham số vùng.
- **Compositor** (`crates/d3d12-backend/src/compositor.rs`): chỉ đặt từng clip rồi chồng các track theo alpha. Chưa có adjustment layer, chưa có bước đọc ảnh đã chồng của các track bên dưới.
- **Hạ tầng tính toán:**
  - Runtime NN viết bằng WGSL trong `gpufx-core/src/nn`, đã dùng cho tách nền.
  - Tải model: `matting_models.rs` (sha256, tiến độ, huỷ).
  - Worker nền có tiến độ và huỷ: proxy, beats.
  - Decoder D3D12VA ra texture GPU, nên tracker phải readback ảnh xám về CPU.
- **Agent và MCP:** đã có `clip_set_transform`, `keyframe_set`.

## Thiết kế dữ liệu (đề xuất)

### Track lưu riêng theo file nguồn

`Project.tracks: Vec<MotionTrack>`, lưu ở cấp project theo file nguồn, giống cách mốc nhịp nhạc đang lưu.

- `MotionTrack { id, media_path, samples, keyed }`
- `TrackSample { source_time, cx, cy, w, h, angle, confidence, manual }`
  - Toạ độ chuẩn hoá theo khung nguồn, đã tính góc xoay hiển thị của file.
  - `manual` đánh dấu frame do người dùng đặt tay.

Overlay gắn vào track qua `Clip.follow: Option<Follow>`:

- `Follow { track_id, ref_clip, anchor_source_time, offset, follow_scale, follow_rotation, smooth }`
- Cách tính khi render ở thời điểm t:
  1. Lấy `ref_clip.source_time_at(t)`.
  2. Nội suy sample của track tại thời gian nguồn đó.
  3. Ánh xạ qua placement và transform của clip video, ra toạ độ khung output.
  4. Cộng offset, rồi cộng keyframe riêng của overlay.
- Offset được tính lúc gắn, sao cho tại frame gắn overlay đứng yên đúng chỗ.
- Làm mượt áp lúc đọc (One-Euro hoặc Gaussian theo thời gian nguồn), không ghi đè dữ liệu gốc.

Lý do không bake thành keyframe dày ngay từ đầu:
- Split, trim, đổi speed, phát ngược và proxy đều giữ đúng vị trí, vì track tính theo thời gian nguồn.
- Chạy lại tracker hay sửa tay thì không phải bake lại.
- Nhiều overlay dùng chung được một track.
- Sửa transform của clip video thì overlay vẫn bám đúng.

Vẫn có lệnh "Bake thành keyframe" cho ai muốn sửa tay từng keyframe.

### Vùng che là một loại clip mới

`ClipKind::Censor { shape: Rect | Ellipse, feather, mode: Blur(r) | Pixelate(block) | Solid(color), follow }`

- Vùng che hoạt động như một adjustment layer, chỉ áp trong vùng của nó. Compositor lấy ảnh đã chồng của các track bên dưới, áp blur hoặc pixelate, rồi trộn lại qua mask hình chữ nhật hoặc elip có feather.
- Khung vùng che kéo vị trí và kích thước bằng gizmo như một sticker.

### Effect có tham số vùng

Ngoài clip che vùng, effect blur/pixelate trên clip video có thêm tham số vùng (hình, vị trí, kích thước, feather, `follow`). Cách này chỉ che chính clip đó, hợp khi muốn vùng che đi theo clip qua split hay di chuyển clip.

## Thuật toán

Chỉ dùng mã và model có giấy phép MIT, Apache hoặc BSD. Tự viết theo bài báo, không chép mã OpenCV contrib.

- **Pha 1, cổ điển, không cần model:**
  - Thuật toán chính: KLT, tức optical flow nhiều tầng trên các điểm góc trong khung. Từ các điểm đó ước lượng phép biến đổi similarity (dịch, scale, xoay), loại điểm nhiễu bằng RANSAC.
  - Phục hồi khi lạc: so khớp mẫu NCC đa tỉ lệ.
  - Chạy CPU trên ảnh xám đã thu nhỏ, đọc từ proxy nếu có.
  - Mỗi frame có độ tin cậy (confidence).
- **Pha 3, model NN:**
  - Ứng viên: NanoTrack (Apache-2.0, khoảng 1–2 MB, khá chắc về giấy phép) và LightTrack (MIT).
  - Phải thêm vào runtime WGSL:
    - cross-correlation depthwise, dùng template làm kernel động lúc chạy;
    - crop và resize theo vùng;
    - giải mã khung bao.
  - Model host trên release của FFmpegGpuFilter (`models-tracking-v1`), giống model tách nền.
- **Pha 4:**
  - Detector khuôn mặt YuNet (MIT) hoặc BlazeFace (Apache) để tự động che mặt.
  - Tracking phối cảnh 4 góc.

## Trải nghiệm người dùng

- **Chọn vật:** inspector của clip video có mục "Tracking" với nút "Chọn vật". Nút này bật chế độ vẽ khung trên preview, dùng lại cơ chế kéo của `placement.rs`.
- **Chạy tracker:** các nút Track tiến, Track lùi, Track 1 frame và Dừng.
  - Tracker chạy ở worker nền, có tiến độ và huỷ được.
  - Preview vẽ đường quỹ đạo.
  - Timeline có dải màu theo độ tin cậy, và chấm ở những frame đặt tay.
- **Sửa chỗ lạc:**
  - Tới frame bị lạc, kéo lại khung để tạo một sample đặt tay, rồi track tiếp.
  - Tracker chạy từ khung tay ra hai phía, dừng ở khung tay kế tiếp.
  - Khi độ tin cậy xuống thấp, tracker dừng và đánh dấu đỏ để người dùng sửa.
- **Gắn overlay:**
  - "Bám theo track…" trên sticker, text hoặc vùng che.
  - Menu "Thêm vùng che bám vật".
  - Mặc định chỉ bám vị trí; scale và xoay là hai checkbox.
- **Agent và MCP:** `track_create`, `track_run` (bất đồng bộ, trả về id công việc), `track_status`, `track_set_sample`, `track_smooth`, `clip_follow_set`, `clip_follow_clear`, `censor_add`, `track_bake_keyframes`.

## Các pha

| Pha | Phạm vi | Chạm tới | Test |
|---|---|---|---|
| P0 | Dữ liệu `MotionTrack`, `Follow`, `ClipKind::Censor` (serde, validate, edit khi split/trim/xoá clip tham chiếu); hàm `follow_transform` trong render/schedule; bước che vùng trong compositor (đọc ảnh các layer dưới, blur/pixelate, mask có feather) | engine: `project.rs`, `render.rs`, `schedule.rs`, `edit.rs`, `validate.rs`; d3d12-backend: `compositor.rs` | Test engine ánh xạ qua split/speed/reverse/`source_in`; test so ảnh mẫu cho vùng che |
| P1 | Tracker cổ điển (KLT + similarity + NCC phục hồi, có confidence); worker đọc frame qua decoder hoặc proxy, readback ảnh xám, tiến độ và huỷ | module mới `engine/src/tracking/` hoặc crate `gpufx-track`; `gpufx-editor/src/tracking.rs` | Chuỗi ảnh tổng hợp (ô vuông dịch, xoay, scale), sai số dưới 1 px; test huỷ |
| P2 | Trải nghiệm: vẽ khung, track tiến/lùi, sửa tay, làm mượt, gắn overlay, undo, bake keyframe, tool agent/MCP, i18n | `ui/inspector.rs`, `ui/placement.rs`, `ui/player.rs`, timeline, `agent/*`, `gpufx-mcp/src/tools.rs` | Test logic gắn và offset, test lệnh agent |
| P3 | Tracker NN (NanoTrack): thêm op vào `gpufx-core/nn` trên nhánh của submodule FFmpegGpuFilter (báo phiên ffmpeggpufilter trước); release model; tổng quát `matting_models.rs` thành bộ tải model chung; chế độ Nhanh/Bền | submodule FFmpegGpuFilter, `matting_models.rs` | So từng op với model gốc; so khung bao trên clip mẫu |
| P4 (tuỳ chọn) | Tự động che mặt (detector + ghép theo IoU), tracking phối cảnh 4 góc | như P3 | |

## Lựa chọn UX tạm ở P2

Các điểm dưới đây do phiên làm P2 tự chọn khi user vắng, chờ user duyệt.

- Track 1 frame chạy tiến một frame từ sample ở frame của đầu phát; đầu phát không tự nhảy theo.
- Các nút track bắt đầu từ sample nằm trên frame của đầu phát (lệch tối đa nửa frame output theo speed). Không có sample ở đó thì dùng sample mặc định của `track_run`.
- Kéo góc khung trên preview luôn đổi kích thước theo trục của ảnh, bỏ qua góc xoay. Khung đã xoay thì chỉ kéo di chuyển được.
- Quỹ đạo vẽ bằng vị trí của clip tại đầu phát, nên khi clip có keyframe vị trí thì đoạn xa đầu phát chỉ gần đúng.
- Làm mượt đặt cho cả track: mọi overlay và vùng effect bám track đó dùng chung một giá trị, `track_smooth` cũng vậy.
- "Thêm vùng che bám vật" nằm trong mục Tracking của clip video. Vùng che dài bằng clip tham chiếu và phủ đúng khung của vật tại đầu phát.
- Gắn overlay neo tại đầu phát, nên overlay đứng yên ở đó và offset luôn là 0. Clip tham chiếu là clip của file đã track đang phát tại đầu phát, nếu không có thì lấy clip gần nhất.
- Bake ghi keyframe X/Y ở mọi frame output, Width khi bám scale, Rotation khi bám xoay. Clip dài hơn 4096 frame thì giãn bước. Animation preset của text sau khi bake chạy sau chuyển động thay vì trước.
- Dải độ tin cậy trên timeline vẽ cho track đang chọn của file. Một sample tô màu khoảng ±0,25 giây nguồn quanh nó; dưới 0,5 là đỏ.

## Lựa chọn tạm ở P3

Các điểm dưới đây do phiên làm P3 tự chọn khi user vắng, chờ user duyệt.

- Model là NanoTrackV2 của [SiamTrackers](https://github.com/HonglinChu/SiamTrackers/tree/master/NanoTrack) (Apache-2.0, cùng file OpenCV `TrackerNano` dùng), ghim ở commit `8fe6791f`. Gói `gpufx_ai_tracking/nanotrack-v2.zip` gồm `tracker.json`, `backbone/`, `head/`, `LICENSE`, `README.md`.
- Ảnh vào backbone theo thứ tự BGR, giá trị 0 đến 255, như bản PyTorch gốc (OpenCV đổi sang RGB). Đo trên clip mẫu thì hai thứ tự cho kết quả gần như nhau.
- Kích thước vùng search là `cạnh template × 255 / 127` (OpenCV chia nguyên ra 2). Phạt cỡ tính theo cỡ khung trước đã nhân tỉ lệ, như bản gốc (OpenCV dùng nhầm toạ độ tâm).
- Điểm của NanoTrack gần như luôn cao kể cả khi vật đã mất, nên độ tin cậy của chế độ Bền là `điểm × (0,5 + 0,5 × NCC)`, NCC là độ giống của khung với mẫu xám (mẫu cập nhật chậm khi chắc chắn). Dưới 0,25 thì coi là mất dấu và dừng.
- Chế độ Bền giữ khung đứng thẳng ở góc của sample bắt đầu, bỏ qua tuỳ chọn xoay.
- Chế độ chọn trong inspector là cài đặt của phiên, không lưu vào project; tool `track_run` mặc định `fast`.
- Tracker Bền dùng một GPU device riêng cho mọi lần chạy trong phiên, tạo khi chạy lần đầu, không dùng chung device của preview.
- Đọc frame vẫn ở 360 dòng như tracker cổ điển.

## Khoá khung theo vật (ổn định theo track)

Ngược với bám: clip video không đứng yên để sticker chạy theo vật, mà cả khung hình chạy ngược lại để vật đứng yên trên màn hình. Vật nghiêng thì cả khung nghiêng theo chiều ngược lại.

- **Dữ liệu:** `Clip.lock: Option<TrackLock>`. `TrackLock` gồm một `TrackLink` (track của chính file nguồn của clip, thời điểm neo theo giờ nguồn, `follow_scale` là giữ cỡ vật, `follow_rotation` là giữ góc vật, làm mượt) và `zoom` (mặc định 1, từ 0,1 đến 8). Vị trí vật luôn được giữ; cỡ và góc chỉ giữ khi bật, mặc định tắt cho khớp Q8.
- **Render:** `Project::lock_pose` tính chỗ vật hiện ra trên khung output lúc `t` và lúc neo qua transform riêng của clip (placement, keyframe, animation), rồi trả `FollowPose` đưa vật từ chỗ hiện tại về chỗ lúc neo, quay và co giãn quanh vật. `schedule::lock_transform` đặt pose đó vào `ClipTransform.follow`, cộng góc vào `rotation`, nên renderer không cần đổi gì.
- **Overlay bám trên clip đã khoá:** `follow_pose` và `clip_frame_mapping` lấy transform của clip tham chiếu đã gồm khoá, nên sticker bám vật vẫn nằm trên vật (vật đứng yên thì sticker cũng đứng yên), khung tracker trên preview cũng trùng vật.
- **Sửa clip:** khoá tính theo giờ nguồn nên split, trim, đổi speed, chạy ngược đều giữ khoá trên mọi mảnh, kể cả mảnh không chứa thời điểm neo. Xoá track hoặc track không còn đúng file thì khoá bị bỏ. Một clip không thể vừa bám vừa khoá.
- **Bake:** `bake_lock` ghi keyframe X/Y, Width khi giữ cỡ hoặc có zoom, Rotation khi giữ góc. `track_bake_keyframes` bake khoá khi clip đang khoá.
- **Giao diện:** trong mục Tracking của clip video có nút "Khoá khung theo vật" (neo tại đầu phát, dùng track đang chọn), khi đã khoá thì có hai ô giữ cỡ, giữ góc, thanh Phóng thêm, nút Bake và Bỏ khoá.
- **Agent và MCP:** `clip_lock_set`, `clip_lock_clear`.

Lựa chọn tạm (chờ user duyệt):

- Mép lộ ra khi khung chạy không tự phóng to để lấp; người dùng tự kéo "Phóng thêm". Phần lộ ra cho thấy track bên dưới (thường là nền đen).
- Khung chỉnh vị trí (placement) trên preview của clip đã khoá vẫn vẽ ở chỗ đặt gốc, chưa gồm khoá; kéo khung là sửa chỗ đặt gốc.
- Clip có animation preset thì bake khoá chỉ gần đúng, vì animation chạy chồng lên keyframe sau khi bake.

## Rủi ro


- **Compositor:** chưa có adjustment layer, phải đổi cách chồng layer. Đây là phần tốn công nhất và ảnh hưởng tới transition.
- **Readback GPU sang CPU:** tracker chạy offline nên tốc độ chấp nhận được, nhưng ảnh xám phải đúng góc xoay hiển thị và chuyển HDR về SDR.
- **Clip nền có transform hoặc keyframe riêng:** ánh xạ phải lấy transform của clip nền tại cùng thời điểm.
- **Vật ra khỏi khung hoặc bị che khuất:** cần ngưỡng độ tin cậy, và quy định overlay làm gì khi lạc: giữ vị trí cuối hay ẩn.

## Câu hỏi cần chốt

User đã chốt cả 10 câu ngày 2026-11-18.

| # | Câu hỏi | Đã chốt |
|---|---|---|
| Q1 | Lưu track riêng hay bake thành keyframe? | Track riêng, có thêm nút Bake (đổ track thành keyframe animation trên overlay) |
| Q2 | Vùng che là clip overlay hay effect trên clip video? | Làm cả hai: clip che vùng riêng (adjustment layer chỉ áp trong vùng) và effect có tham số vùng trên clip video |
| Q3 | Hình dạng vùng che | Khung kéo vị trí và kích thước bằng gizmo như sticker; hình chữ nhật hoặc elip, có feather, chỉnh trong inspector |
| Q4 | P1 bám những gì? | Vị trí, scale và xoay; xoay bật/tắt được. Phối cảnh 4 góc để P4 |
| Q5 | Có chạy tracker trên proxy khi có proxy không? | Có |
| Q6 | Model NN ở P3 | NanoTrack, host trên release FFmpegGpuFilter |
| Q7 | Khi lạc thì làm gì? | Dừng, đánh dấu đỏ để sửa tay |
| Q8 | Overlay mặc định bám scale và xoay không? | Mặc định chỉ bám vị trí; scale và xoay là hai checkbox riêng |
| Q9 | Tự động che mặt | Để P4, dùng YuNet |
| Q10 | Track lưu ở cấp project theo file nguồn hay theo từng clip? | Cấp project theo file nguồn |
