# Backlog

### [Vừa] Cần bản FFmpeg shared GPLv3 cho win64

Editor đã chọn FFmpeg GPLv3 để xuất bằng x264. Release `ffmpeg-N-128691-gcc583787bb` của tqk2811/FFmpegGpuFilter chỉ có bản LGPL. Cần asset win64 biến thể `gpl` cùng commit cc583787bb, có header và thư viện. User đồng ý tạo release ngày 2026-11-09, phiên FFmpegGpuFilter đã đóng nên yêu cầu chưa gửi được. Chặn bước test xuất x264 của `docs/plans/spec-engine-phase1-vi.md`. Ghi ngày 2026-11-09.

### [Thấp] Yêu cầu API gpufx đã gửi, chưa rõ đã xử lý chưa

Hai yêu cầu cho `external/FFmpegGpuFilter`: công khai `GpuContext::from_device` (`crates/gpufx-core/src/runtime.rs:267`) để app và gpufx dùng một wgpu device; cho zero-copy đọc texture lớn hơn khung (`crates/gpufx-core/src/d3d12.rs:365`) vì D3D12VA cấp 1920x1088 cho khung 1080p. Đã gửi phiên FFmpegGpuFilter, chưa có phản hồi. Ghi ngày 2026-11-09.

### [Vừa] Tua clip GOP dài vượt ngưỡng đề xuất

Clip H.264 GOP 250 có B-frame tua p95 502 ms so với ngưỡng đề xuất 200 ms (`docs/plans/result-poc-phase0-vi.md`). Footage thật thường có GOP dài, nên giai đoạn sau cần proxy, cache hoặc giải mã trước. Ghi ngày 2026-11-09.

### [Vừa] Chờ decoder rảnh trước mỗi lần mix làm mất giải mã song song

`D3d12Decoder::wait_until_idle` ở `crates/poc/src/decoder.rs` chờ CPU trước mỗi `mix` để khung không còn bị decode khác tham chiếu. Phát p99 có lần lên 31,4 ms, sát ngưỡng 33 ms, chưa rõ nguyên nhân. Bản editor cần đồng bộ trên GPU hoặc tách luồng. Ghi ngày 2026-11-09.

### [Thấp] Fence theo khung đã cấp phát, chưa theo khung đã submit

`crates/poc/src/decoder.rs:52-58` theo dõi khung qua `get_buffer2`. Nếu cấp phát xong mà không submit (slice hỏng), fence mới nhất có thể không phủ các decode trước đó. Chưa gặp với clip H.264 đã thử; điều này suy từ cách FFmpeg hoạt động, chưa kiểm chứng. Ghi ngày 2026-11-09.

### [Thấp] Chưa cô lập nguồn lỗi validation #1334

Debug layer báo #1334 (CopyTextureRegion trên COPY list) 24 lần. Số lần khớp bước đọc ngược pixel `av_hwframe_transfer_data` trong kiểm tra, nhưng chưa cô lập (`POC_D3D12_DEBUG=1`). Ghi ngày 2026-11-09.

### [Thấp] Benchmark chưa kiểm hết tính đúng của ảnh và chưa có test tự động

Blend chỉ kiểm ở 3 vị trí tua, ma trận màu lấy cùng nguồn với gpufx, khung lúc phát chưa kiểm, player chưa thao tác bằng tay. Workspace chưa có bộ test nên bản sửa đồng bộ chưa có test hồi quy; nên dựng ở giai đoạn 1 (`crates/poc/src/mix_bench.rs`). Ghi ngày 2026-11-09.
