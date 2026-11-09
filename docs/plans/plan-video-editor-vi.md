# Kế hoạch GpufxVideoEditor

Ngày lập: 2026-11-09. Trạng thái: **đề xuất, chưa bắt đầu code**.

Kế hoạch dựa trên việc đọc docs và header C của FFmpegGpuFilter, cùng khảo sát web. Chưa có POC nào được chạy. Nhiều thông tin về các project khác đến từ nguồn thứ cấp, cần kiểm lại trước khi dựa vào.

## Mục tiêu

Dựng một trình dựng video theo timeline (non-linear editor), dùng `gpufx-core` làm engine hiệu ứng trên GPU. FFmpegGpuFilter nằm ở submodule `external/FFmpegGpuFilter` (submodule FFmpeg lồng bên trong chưa được kéo về, vì editor chỉ cần `gpufx-core`).

Không dựng editor bằng cách sinh lệnh FFmpeg cho preview: filter graph chạy một chiều, khó tua và khó đổi tức thời.

## Quyết định

| Vấn đề | Lựa chọn | Trạng thái |
|---|---|---|
| Tên project | GpufxVideoEditor | Đã chốt |
| Cách dùng thư viện | FFmpegGpuFilter làm submodule | Đã chốt |
| Nền tảng | Windows và Linux; web nếu làm được | Đã chốt |
| UI | Rust + egui, dùng chung wgpu device nên preview không qua interop | Đã chốt |
| Đối tượng | Kiểu CapCut (nhanh, nhiều hiệu ứng) | Đã chốt |
| Giấy phép code | Mã nguồn của repo MIT | Đã chốt |
| FFmpeg | Build song song hai bản: GPLv3 (có x264/x265) và LGPL. Bản kèm FFmpeg GPL phải phân phối theo GPL, gồm cả mã nguồn; bản LGPL theo điều kiện LGPL | Đã chốt |

## Các editor open-source đang làm gì

| Project | Cách làm | Điểm đáng chú ý |
|---|---|---|
| Kdenlive | Dựa trên MLT | Maintainer nói (12/2025) MLT chưa dùng GPU cho hiệu ứng; GPU effect còn nằm trong roadmap |
| Shotcut | Dựa trên MLT | Có chế độ Linear 10-bit GPU/CPU còn thử nghiệm; HDR 26.6 cần chế độ GPU; hw decode từ 26.1 |
| Blender VSE | Tận dụng compositor có sẵn | Đang đưa dần pipeline lên GPU; hw encode/decode vẫn trong kế hoạch |
| Olive 0.2 | Node-based | Viết lại nhiều năm, vẫn chưa phát hành chính thức |
| Pitivi | GStreamer Editing Services | Tách thư viện timeline khỏi app, có tích hợp OpenTimelineIO |
| OpenCut | Web, TypeScript | Đang viết lại với lõi Rust; renderer được cho là chuyển sang wgpu/WASM |
| FilmCraft | Rust + wgpu, tự viết codec | Còn alpha; thời gian là số nguyên; mọi thao tác là lệnh; audio làm đồng hồ chính |
| Eddie, Gausian | Rust + egui + wgpu | Gần nhất với hướng đề xuất; Eddie dùng FFmpeg |

Bài học:

- Editor lâu đời khó chuyển lên GPU vì pipeline CPU đã ăn sâu. Project này đi từ GPU trước nên có lợi thế.
- Olive cho thấy rủi ro lớn nhất là phạm vi quá rộng. Giai đoạn đầu phải giữ thật gọn.
- Các project mới đều tách engine khỏi UI và dùng thời gian số nguyên để tránh lệch khung.

## FFmpegGpuFilter đã có gì

- Gói hiệu ứng nạp ngoài (`.zip`), với khoảng 150 mục trong `packages/gpufx/capcut`.
- Chuyển format trên GPU, AI upscale và nội suy khung hình.
- Đường frame Vulkan/D3D giữ zero-copy, cho phép dùng device do app sở hữu (`gpufx_effect_use_vulkan`, `gpufx_effect_use_d3d12`).
- Bật/tắt node và đổi tham số lúc chạy (`gpufx_effect_set_param`, `gpufx_effect_set_node_enabled`).
- Hiệu ứng không giữ trạng thái giữa các frame, nên tua dễ đúng.

## Giới hạn hiện tại ảnh hưởng tới editor

Chi tiết ở `external/FFmpegGpuFilter/docs/LIMITATIONS-vi.md`.

- Màu mới có matrix và range, chưa có transfer function; HDR bị coi như SDR.
- Đổi kích thước input giữa chừng là lỗi. Compositor phải đưa mọi clip về cùng kích thước trước.
- Một lần submit lỗi làm hỏng device tới hết tiến trình, trong khi editor cần sống sót qua effect lỗi.
- Linux GPU thật, AMD và macOS chưa từng được test.
- AI upscale không realtime, phải render trước rồi cache.

## Kiến trúc đề xuất

| Lớp | Trách nhiệm | Dùng |
|---|---|---|
| UI | Timeline, preview, inspector; chỉ phát lệnh xuống engine | egui (đề xuất) |
| Project model | Track, clip, keyframe, thời gian số nguyên, undo, lưu JSON | Viết mới |
| Scheduler | Tính frame nào cần decode, cache frame, proxy | Viết mới |
| Decode / encode | Hw decode lên texture, xuất file | libav* |
| Compositor | Ghép nhiều layer, gọi hiệu ứng | `gpufx-core` (wgpu); phần layer viết mới, có thể tái dùng gói như `fit` nhưng chưa rà hết |
| Audio | Mix, fade, làm đồng hồ chính cho preview | Viết mới |

## Lộ trình

Giai đoạn 0 là cửa quyết định, không đạt thì dừng hoặc đổi hướng.

0. **POC:** 2-3 clip 1080p, hw decode lên texture, chạy một transition, hiển thị trên cùng wgpu device. Đo tốc độ tua, độ trễ và VRAM.
1. **Engine không UI:** thời gian số nguyên, project JSON, compositor, xuất file bằng CLI. Render lại phải cho kết quả trùng khớp để làm test hồi quy.
2. **Audio:** cắt, mix, fade, audio làm đồng hồ chính cho preview.
3. **UI tối thiểu:** cắt, kéo clip, nhiều track, chọn hiệu ứng từ manifest, undo.
4. **Hoàn thiện:** keyframe, text, proxy, cache, phục hồi khi lỗi.
4b. **MCP server cho agent:** chạy trên project file và điều khiển editor đang mở, chụp hình theo thời điểm hoặc frame index. Đặc tả ở [spec-mcp-server-vi.md](spec-mcp-server-vi.md).
5. **Mở rộng:** quản lý màu/HDR, render trước cho AI, import/export OpenTimelineIO, có thể thêm OpenFX.

Ước lượng thô: bản dựng được video ngắn cần vài tháng; cạnh tranh với Kdenlive cần nhiều năm.

## Rủi ro giấy phép và pháp lý

Đây là hiểu biết chung, không phải tư vấn pháp lý.

- **Bản quyền hiệu ứng:** các gói `capcut` là bản dựng lại hiệu ứng của CapCut. Nếu phát hành trong sản phẩm, cần xem lại về pháp lý hoặc thay bằng bộ hiệu ứng riêng.
- **GPLv3:** được bán, nhưng người nhận bản phát hành phải được nhận mã nguồn toàn bộ sản phẩm theo GPL. Không hợp với app đóng mã, và thường xung đột với điều khoản Apple App Store.
- **FFmpeg LGPL:** app đóng mã dùng được nếu link động, kèm thông báo giấy phép, và người dùng thay được DLL.
- **FFmpeg có GPL (libx264, libx265, một số filter):** cả sản phẩm phân phối chung bị ràng buộc theo GPL. Build mặc định của FFmpegGpuFilter (`FFMPEG_FILTERS=all`) có filter GPL. x264 có bán license thương mại riêng.
- **Bằng sáng chế H.264/HEVC:** là chuyện riêng với giấy phép mã nguồn, vẫn cần xem xét.
- **Tên project:** mới kiểm GitHub và crates.io, chưa tra nhãn hiệu hay tên miền. Tên mô tả chung khó đăng ký nhãn hiệu.

## Bước tiếp theo

1. Chốt các mục "chờ chốt" ở bảng quyết định.
2. Viết đặc tả POC giai đoạn 0.

## Nguồn

- [Kdenlive roadmap](https://kdenlive.org/roadmap/), [State of Kdenlive 2026](https://kdenlive.org/news/2026/state-2026/), [Bug 512987](https://bugs.kde.org/show_bug.cgi?id=512987)
- [Shotcut 26.6](https://shotcut.org/blog/new-release-26.6.25/), [Shotcut 25.12](https://alternativeto.net/news/2026/1/shotcut-25-12-adds-10-bit-video-cpu-pipeline-linear-color-processing-and-ui-upgrades), [Shotcut 26.1 beta](https://phoronix.com/news/shotcut-26.1-beta)
- [Blender VSE 2026 roadmap](https://devtalk.blender.org/t/video-sequence-editor-vse-2026-roadmap/43206), [More Blender VSE tidbits](https://aras-p.info/blog/2026/08/18/More-Blender-VSE-tidbits/)
- [Olive (Wikipedia)](https://en.wikipedia.org/wiki/Olive_(software))
- [GES](https://developer.pitivi.org/GES.html), [Pitivi planet](https://pitivi.org/planet)
- [OpenCut explained](https://mer.vin/2026/07/opencut-explained-open-source-capcut-alternative-classic-editor-vs-ground-up-rewrite/), [OpenCut listing](https://openalternative.co/opencut)
- [FilmCraft](https://github.com/storytold/filmcraft), [Eddie](https://github.com/BurgessTG/eddie), [Gausian](https://github.com/gausian-AI/Gausian_native_editor)
