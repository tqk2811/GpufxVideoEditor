# Kế hoạch: Object tracking (sticker, text, che vùng bám theo vật)

Mục tiêu: người dùng chọn một vật trong clip video. Sticker, text hoặc vùng che (blur, pixelate, hộp màu) gắn vào vật đó và tự di chuyển theo, có thể bám cả scale và góc xoay. Tham khảo: Tracking của CapCut, Motion tracking của DaVinci Resolve, Motion Tracker của Shotcut, tracker OpenCV trong Kdenlive.

Trạng thái: đã duyệt, các câu hỏi đã chốt ở [Câu hỏi cần chốt](#câu-hỏi-cần-chốt). P0 và P1 đã xong: tracker cổ điển ở `crates/engine/src/tracker/`, worker nền ở `crates/gpufx-editor/src/tracking.rs`, tool `track_run`, `track_status`, `track_cancel`. P2 đã xong: mục Tracking trong inspector của clip video (`ui/tracking_inspector.rs`), vẽ quỹ đạo và sửa khung trên preview (`ui/tracking_overlay.rs`), dải độ tin cậy trên timeline, thao tác ở `track_edit.rs`, tool `track_create`, `track_smooth`, `track_bake_keyframes`, `track_run` với `direction: both`. P3 đã xong trên nhánh, trừ release model: op `correlation` và activation `exp` trong `gpufx-core/nn`, `NanoTrack` (`gpufx-core/src/nn/track.rs`, cắt ảnh và giải mã khung bao bằng WGSL), script `scripts/convert-tracking-models.py` của FFmpegGpuFilter, bộ tải model chung `model_download.rs`, model tracker ở `tracking_models.rs`, chế độ Nhanh/Bền trong inspector và tham số `mode` của `track_run`. Còn chờ user tạo release `models-tracking-v1` rồi ghi `sha256_zip`, xem [Lựa chọn ở P3](#lựa-chọn-ở-p3).

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
| P4 (tuỳ chọn) | Tự động che mặt (detector + ghép theo IoU), tracking phối cảnh 4 góc. Phần 1 (che mặt) đã làm, xem mục "Tự động che mặt"; phần 2 (phối cảnh 4 góc) đã làm, xem mục "Tracking phối cảnh 4 góc" | như P3; `gpufx-core/nn/face.rs`, `engine/src/faces.rs`, `gpufx-editor/src/face_censor.rs`; `engine/src/tracker/homography.rs`, `tracker/planar.rs`, `d3d12-backend/src/pinned_place.rs` | So từng đầu ra với onnxruntime; test ghép IoU; test đầu cuối với detector giả và với YuNet thật |

## Lựa chọn UX ở P2

Các điểm dưới đây do phiên làm P2 tự chọn khi user vắng, user đã duyệt.

- Track 1 frame chạy tiến một frame từ sample ở frame của đầu phát; đầu phát không tự nhảy theo.
- Các nút track bắt đầu từ sample nằm trên frame của đầu phát (lệch tối đa nửa frame output theo speed). Không có sample ở đó thì dùng sample mặc định của `track_run`.
- Kéo góc khung trên preview luôn đổi kích thước theo trục của ảnh, bỏ qua góc xoay. Khung đã xoay thì chỉ kéo di chuyển được.
- Quỹ đạo vẽ bằng vị trí của clip tại đầu phát, nên khi clip có keyframe vị trí thì đoạn xa đầu phát chỉ gần đúng.
- Làm mượt đặt cho cả track: mọi overlay và vùng effect bám track đó dùng chung một giá trị, `track_smooth` cũng vậy.
- "Thêm vùng che bám vật" nằm trong mục Tracking của clip video. Vùng che dài bằng clip tham chiếu và phủ đúng khung của vật tại đầu phát.
- Gắn overlay neo tại đầu phát, nên overlay đứng yên ở đó và offset luôn là 0. Clip tham chiếu là clip của file đã track đang phát tại đầu phát, nếu không có thì lấy clip gần nhất.
- Bake ghi keyframe X/Y ở mọi frame output, Width khi bám scale, Rotation khi bám xoay. Clip dài hơn 4096 frame thì giãn bước. Animation preset của text sau khi bake chạy sau chuyển động thay vì trước.
- Dải độ tin cậy trên timeline vẽ cho track đang chọn của file. Một sample tô màu khoảng ±0,25 giây nguồn quanh nó; dưới 0,5 là đỏ.

## Lựa chọn ở P3

Các điểm dưới đây do phiên làm P3 tự chọn khi user vắng, user đã duyệt.

- Model là NanoTrackV2 của [SiamTrackers](https://github.com/HonglinChu/SiamTrackers/tree/master/NanoTrack) (Apache-2.0, cùng file OpenCV `TrackerNano` dùng), ghim ở commit `8fe6791f`. Gói `gpufx_ai_tracking/nanotrack-v2.zip` gồm `tracker.json`, `backbone/`, `head/`, `LICENSE`, `README.md`.
- Ảnh vào backbone theo thứ tự BGR, giá trị 0 đến 255, như bản PyTorch gốc (OpenCV đổi sang RGB). Đo trên clip mẫu thì hai thứ tự cho kết quả gần như nhau.
- Kích thước vùng search là `cạnh template × 255 / 127` (OpenCV chia nguyên ra 2). Phạt cỡ tính theo cỡ khung trước đã nhân tỉ lệ, như bản gốc (OpenCV dùng nhầm toạ độ tâm).
- Điểm của NanoTrack gần như luôn cao kể cả khi vật đã mất, nên độ tin cậy của chế độ Bền là `điểm × (0,5 + 0,5 × NCC)`, NCC là độ giống của khung với mẫu xám (mẫu cập nhật chậm khi chắc chắn). Dưới 0,25 thì coi là mất dấu và dừng.
- Chế độ Bền giữ khung đứng thẳng ở góc của sample bắt đầu, bỏ qua tuỳ chọn xoay.
- Chế độ chọn trong inspector là cài đặt của phiên, không lưu vào project; tool `track_run` mặc định `fast`.
- Tracker Bền dùng một GPU device riêng cho mọi lần chạy trong phiên, tạo khi chạy lần đầu, không dùng chung device của preview.
- Đọc frame vẫn ở 360 dòng như tracker cổ điển.

## Tự động che mặt (P4 phần 1)

- **Model:** YuNet `face_detection_yunet_2023mar.onnx` của [OpenCV Zoo](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet), giấy phép MIT (README của thư mục ghi mọi file trong đó, gồm cả trọng số, theo MIT), ghim ở commit `eb9f4f2d` kèm sha256. Gói `gpufx_ai_tracking/yunet-2023mar.zip` gồm `model.json`, `weights.bin`, `io.json` (role `face_detector`), `LICENSE`, `README.md`; phát hành chung release `models-tracking-v1` với NanoTrack.
- **Chạy model:** `scripts/convert-tracking-models.py` cắt graph trước các Reshape ở đầu ra (bỏ nhánh landmark), pnnx với hai cỡ ảnh để Resize đi theo tỉ lệ, rồi `gpufx-nnconv`. Không cần thêm op. `FaceDetector` của gpufx-core co cạnh dài của frame về 640, đệm số 0 tới bội 32, chạy trên GPU, đọc về các map của stride 8, 16, 32, giải mã khung và NMS trên CPU. Test so từng đầu ra với onnxruntime (CPU reference và GPU) và so khung tìm được trên một frame của ảnh mẫu OpenCV Zoo.
- **Ghép theo IoU:** `engine::faces::face_tracks` gán khung của mỗi frame cho đường đang mở theo IoU lớn nhất trước. Khung không đường nào nhận thì mở đường mới. Đường lỡ quá `max_gap` frame liền thì đóng. Đường ngắn hoặc điểm trung bình thấp bị bỏ.
- **Luồng chạy:** worker đọc frame như tracker (proxy nếu có, 360 dòng), có tiến độ và huỷ. Khi xong, mỗi đường thành một `MotionTrack` (sample có confidence là điểm của detector) và một clip che vùng bám theo nó, tất cả trong một bước undo.
- **Giao diện và agent:** nút "Tự động che mặt" trong mục Tracking của clip video, có thanh tiến độ và nút dừng, kèm tải hoặc import model khi chưa có. Tool `censor_faces` (bất đồng bộ như `track_run`), `censor_faces_status`, `censor_faces_cancel`.

Lựa chọn (user đã duyệt):

- Dò tối đa 10 frame mỗi giây nguồn; các frame giữa lấy nội suy của motion track.
- Ngưỡng điểm 0,6, NMS 0,3. Ghép: IoU từ 0,3, cho lỡ tối đa 5 lần dò liền, giữ đường có từ 5 lần dò và điểm trung bình từ 0,7.
- Vùng che to gấp 1,3 lần khung mặt quanh tâm, hình elip, feather 0,3, chế độ blur bán kính 30. Bám vị trí và cỡ (không bám xoay), làm mượt 0,15 giây.
- Vùng che kéo dài từ lần dò đầu tới lần dò cuối của mặt cộng thêm một bước dò, cắt trong khoảng của clip.
- Huỷ giữa chừng thì không thêm gì, kể cả các mặt đã dò được.
- Model YuNet nằm cùng thư mục `gpufx_ai_tracking` và dùng chung bộ tải của tracker; `tracking_models.rs` nay quản lý cả hai model.
- Dùng chung GPU device với tracker Bền.
- Ảnh mẫu của golden test (`largest_selfie.jpg` của OpenCV Zoo) chỉ được script tải về `build/` để test, không đưa vào repo hay gói.

## Tracking phối cảnh 4 góc (P4 phần 2)

Dùng cho vật phẳng như màn hình, biển hiệu, trang giấy: track theo bốn góc thay vì một khung, rồi ghim bốn góc của overlay vào bốn góc của vật (corner pin).

- **Dữ liệu:** `TrackSample.quad` (tuỳ chọn, mặc định không có) là bốn góc theo thứ tự trên trái, trên phải, dưới phải, dưới trái của vật, tính theo phần của ảnh nguồn. Khung (`cx`, `cy`, `w`, `h`, `angle`) vẫn được ghi, lấy từ bốn góc (tâm là trung bình bốn góc, cạnh là trung bình hai cạnh đối, góc là góc cạnh trên), nên mọi chỗ chỉ đọc khung vẫn chạy. Validate: toạ độ hữu hạn, trong khoảng ±8, tứ giác lồi và đúng thứ tự. Một track là "phẳng" khi có sample mang `quad`. `MotionTrack::quad_at` nội suy và làm mượt bốn góc như `point_at`; sample không có `quad` cho bốn góc của khung xoay.
- **Tracker:** `PlanarTracker` (chỉ chế độ Nhanh). Mỗi bước lấy điểm góc trong tứ giác, theo dõi bằng KLT nhiều tầng kèm kiểm tra đi về, khớp homography bằng RANSAC (DLT chuẩn hoá Hartley, mẫu 4 điểm, khớp lại bằng bình phương nhỏ nhất trên điểm đồng thuận), rồi dời bốn góc. Độ tin cậy là NCC giữa mẫu (tứ giác nắn về hình vuông 24×24) với mẫu nhớ, nhân căn tỉ lệ điểm đồng thuận. Dưới ngưỡng thì tìm lại bằng NCC trên tứ giác nắn, dời và co giãn quanh chỗ dự đoán (giữ hình dạng); không thấy thì dừng và đánh dấu lạc (Q7). Run bắt đầu từ sample có `quad` luôn dùng tracker này, kể cả khi đang chọn Bền.
- **Bám:** `Follow.perspective` (mặc định tắt). `Project::follow_pin` tính homography, theo phần của khung output, đưa bốn góc của vật nhìn qua clip tham chiếu lúc neo tới bốn góc lúc `t` (qua placement, keyframe, animation và khoá của clip tham chiếu ở từng thời điểm), rồi cộng offset. `schedule::follow_transform` đặt nó vào `ClipTransform.pin`. Bốn góc của overlay là bốn góc của hình chữ nhật đặt của nó (chưa có bám, đã xoay theo góc riêng) đi qua homography (`ClipTransform::pinned_corners`). Khi bật phối cảnh thì pose bám cũng tính cả scale và xoay, để gizmo và khung trên preview gần đúng.
- **Compositor:** gói dựng sẵn `pinned_place` lấy mỗi pixel output ngược qua homography nghịch về ảnh nguồn, nên lấy mẫu đúng phối cảnh (không chia hai tam giác). Màu lọc theo premultiplied, mép mịn theo khoảng cách tới cạnh tính bằng pixel output. Ma trận được nhân dấu để toạ độ thứ ba dương trong ảnh; pixel bên kia đường chân trời của mặt phẳng không hiện gì. Bốn góc không lồi thì quay về cách đặt hình chữ nhật cũ.
- **Giao diện:** mục Tracking có ô "Phối cảnh 4 góc". Bật thì khung tại đầu phát thành sample đặt tay có bốn góc; tắt thì mọi sample bỏ bốn góc (một bước undo). Preview vẽ tứ giác và hai đường chéo; kéo thân là dời cả bốn góc, kéo một góc là dời riêng góc đó, không cho kéo thành tứ giác gập. Overlay đang bám có ô "Phối cảnh".
- **Agent và MCP:** `track_create` và `track_set_sample` nhận `quad` (khi đó khung lấy từ bốn góc, `cx`, `cy`, `w`, `h` thành tuỳ chọn); `clip_follow_set` nhận `perspective`.
- **Vùng che theo tứ giác:** clip che vùng bám track với "Phối cảnh" che đúng tứ giác đã ghim (`CensorRegion.quad` lấy từ `ClipTransform::pinned_corners`). Vùng của effect có `EffectRegion.perspective`; khi track liên kết là track phẳng thì `tracking::follow_region_quad` đưa bốn góc của khung (đã xoay) qua homography từ bốn góc lúc neo tới bốn góc lúc `t`, track không phẳng thì vẫn bám theo khung. Shader `censor_mix` lấy mỗi pixel ngược qua homography nghịch về hình chữ nhật đơn vị, rồi tính mép (chữ nhật hoặc elip, feather) theo pixel của một khung có cỡ bằng trung bình các cạnh đối của tứ giác. Inspector có ô "Phối cảnh" dưới chỗ chọn track của vùng effect khi track đó phẳng.
- **Khoá khung phối cảnh:** `TrackLock.perspective` (chỉ cho track phẳng, validate từ chối track không có bốn góc). `Project::lock_pin` tính homography đưa bốn góc của vật lúc `t` (qua transform riêng của clip, chưa khoá) về bốn góc lúc neo, rồi phóng theo `zoom` quanh tâm bốn góc; `schedule::lock_transform` đặt nó vào `ClipTransform.pin`, nên compositor dùng lại đường corner pin. `FrameMapping` mang theo `pin`, nên overlay bám qua clip đã khoá phối cảnh và khung tracker trên preview vẫn nằm trên vật. Bake từ chối (`bake_perspective`), như bám phối cảnh. Mục khoá có ô "Phối cảnh" (tắt khi track không phẳng); `clip_lock_set` nhận `perspective`.

Lựa chọn (user đã duyệt):

- Track phẳng chỉ chạy được ở chế độ Nhanh: run bắt đầu từ sample có bốn góc luôn chạy Nhanh, và inspector tắt lựa chọn Bền cho run đó.
- Tracker phẳng lấy tối đa 200 điểm góc (gấp đôi tracker khung), cần ít nhất 8 điểm đồng thuận. Một bước mà diện tích đổi quá 4 lần hoặc tứ giác bị gập thì coi như luồng quang học thất bại và chuyển sang tìm lại.
- Tìm lại chỉ dời và co giãn tứ giác, không thử đổi phối cảnh; vật đổi phối cảnh mạnh trong lúc bị che thì dừng lạc để sửa tay.
- Bật "Phối cảnh" trên overlay thì bỏ qua hai ô bám scale và bám xoay (phối cảnh đã gồm cả hai). Bake keyframe của overlay phối cảnh chỉ ghi phần similarity (X/Y, Width, Rotation); keyframe không biểu diễn được phối cảnh.
- Kéo góc của tứ giác dùng ánh xạ của clip tại đầu phát, như kéo khung.

- Mép mịn của vùng che tứ giác tính theo khung có cỡ bằng trung bình các cạnh, không theo độ co giãn phối cảnh tại từng pixel, nên phía xa của mặt phẳng có mép mịn hơi rộng hơn mắt thấy.
- Khoá phối cảnh bỏ qua hai ô giữ cỡ và giữ góc (phối cảnh đã gồm cả hai). Pose dùng cho gizmo của clip đã khoá phối cảnh vẫn là pose khung; overlay bám clip này qua `follow_pose` (khi không bật phối cảnh) chỉ gần đúng, còn `follow_pin` thì đúng.
- Vùng effect bật phối cảnh mà track không phẳng thì cờ vẫn được lưu nhưng vùng bám theo khung.

Việc còn lại:

- Chưa có test đầu cuối chạy tracker phẳng qua worker của editor trên video thật.

## Khoá khung theo vật (ổn định theo track)

Ngược với bám: clip video không đứng yên để sticker chạy theo vật, mà cả khung hình chạy ngược lại để vật đứng yên trên màn hình. Vật nghiêng thì cả khung nghiêng theo chiều ngược lại.

- **Dữ liệu:** `Clip.lock: Option<TrackLock>`. `TrackLock` gồm một `TrackLink` (track của chính file nguồn của clip, thời điểm neo theo giờ nguồn, `follow_scale` là giữ cỡ vật, `follow_rotation` là giữ góc vật, làm mượt) và `zoom` (mặc định 1, từ 0,1 đến 8). Vị trí vật luôn được giữ; cỡ và góc chỉ giữ khi bật, mặc định tắt cho khớp Q8.
- **Render:** `Project::lock_pose` tính chỗ vật hiện ra trên khung output lúc `t` và lúc neo qua transform riêng của clip (placement, keyframe, animation), rồi trả `FollowPose` đưa vật từ chỗ hiện tại về chỗ lúc neo, quay và co giãn quanh vật. `schedule::lock_transform` đặt pose đó vào `ClipTransform.follow`, cộng góc vào `rotation`, nên renderer không cần đổi gì.
- **Overlay bám trên clip đã khoá:** `follow_pose` và `clip_frame_mapping` lấy transform của clip tham chiếu đã gồm khoá, nên sticker bám vật vẫn nằm trên vật (vật đứng yên thì sticker cũng đứng yên), khung tracker trên preview cũng trùng vật.
- **Sửa clip:** khoá tính theo giờ nguồn nên split, trim, đổi speed, chạy ngược đều giữ khoá trên mọi mảnh, kể cả mảnh không chứa thời điểm neo. Xoá track hoặc track không còn đúng file thì khoá bị bỏ. Một clip không thể vừa bám vừa khoá.
- **Bake:** `bake_lock` ghi keyframe X/Y, Width khi giữ cỡ hoặc có zoom, Rotation khi giữ góc. `track_bake_keyframes` bake khoá khi clip đang khoá.
- **Giao diện:** trong mục Tracking của clip video có nút "Khoá khung theo vật" (neo tại đầu phát, dùng track đang chọn), khi đã khoá thì có hai ô giữ cỡ, giữ góc, thanh Phóng thêm, nút Bake và Bỏ khoá.
- **Agent và MCP:** `clip_lock_set` (có `perspective`, xem [Tracking phối cảnh 4 góc](#tracking-phối-cảnh-4-góc-p4-phần-2)), `clip_lock_clear`. Vùng của effect đặt qua `effect_region_set` (hình, khung, feather, track liên kết với `follow_scale`, `follow_rotation`, `perspective`, neo tại `at`) và `effect_region_clear`; `set_effect_region` từ chối liên kết tới track không tồn tại hoặc của file khác thay vì lặng lẽ bỏ.
- **Phím:** Esc tắt chế độ "Chọn vật" và bỏ luôn khung đang kéo dở (id vùng vẽ đổi theo `tracking_pick_round`, nên lần chọn sau không nhận cú nhả chuột cũ).
- **Chế độ tracker:** sample mà lần chạy sẽ bắt đầu có bốn góc thì inspector tắt lựa chọn Bền, tooltip giải thích track phẳng luôn chạy Nhanh.

Lựa chọn (user đã duyệt):

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
