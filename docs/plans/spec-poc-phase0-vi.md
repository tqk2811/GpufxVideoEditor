# Đặc tả POC giai đoạn 0

Ngày lập: 2026-11-09. Trạng thái: **đang thực hiện**. Kế hoạch tổng ở [plan-video-editor-vi.md](plan-video-editor-vi.md).

POC trả lời một câu hỏi: trên Windows, có thể giải mã phần cứng nhiều clip 1080p, chạy transition của `gpufx-core` và hiển thị trong egui với tốc độ đủ cho một editor hay không.

## Quyết định tạm

Các mục này lấy theo phương án đề xuất của kế hoạch để làm tiếp. Chỉ user mới chốt được.

| Mục | Lựa chọn tạm |
|---|---|
| Nền tảng | Windows, D3D12 |
| UI | Rust + egui, qua wgpu 30 |
| Đối tượng | Kiểu CapCut |
| FFmpeg | Bản LGPL shared `ffmpeg-N-128691-gcc583787bb-win64-lgpl-shared.zip`, link động |

## Phạm vi

Có:

- 3 clip H.264 1080p30 tự sinh, cùng kích thước, mỗi clip dài 20 giây, GOP 30 khung.
- Giải mã D3D12VA qua libav*.
- Transition `packages/gpufx/capcut/mix` (`first`, `second`, tham số `progress` từ 0 đến 1).
- Một cửa sổ egui hiển thị kết quả, có thanh tua.
- Chế độ benchmark tự chạy, ghi kết quả JSON.

Không có: audio, timeline, xuất file, lưu project, resize input, HDR, phục hồi khi mất device.

## Thiết kế

1. App tạo wgpu Dx12 device cho egui và lấy `ID3D12Device` thật của device đó.
2. Decoder D3D12VA dùng chung `ID3D12Device` qua `AVD3D12VADeviceContext`.
3. `GpuContext::from_d3d12` nhận `ID3D12Device` đó.
4. Mỗi khung, `EffectGraph::process_d3d12` nhận hai `D3d12Frame` NV12 lấy từ `AVD3D12VAFrame`, cùng texture RGBA đầu ra.
5. Texture RGBA đầu ra được tạo bằng D3D12, nhập vào device của app bằng `create_texture_from_hal`, đồng bộ bằng fence, rồi egui vẽ nó.

Đường vòng ở bước 3 và 5 tồn tại vì `GpuContext::from_device` chưa công khai. Vì vậy POC dùng chung một `ID3D12Device` nhưng có hai wgpu device. Yêu cầu mở API đã gửi cho phiên FFmpegGpuFilter, submodule giữ nguyên ở 877149b.

Decoder có thể cấp phát texture array, khi đó `gpufx-core` sao chép một lần cho mỗi plane. Benchmark phải ghi rõ khung đi đường zero-copy hay sao chép.

## Đo lường

Máy đo: RTX 4060 Laptop. Mỗi phép đo ghi p50, p95, p99 và số mẫu.

| Chỉ số | Cách đo | Ngưỡng đạt đề xuất |
|---|---|---|
| Tốc độ tua | Từ lúc yêu cầu tới lúc khung đúng sẵn sàng, 200 vị trí ngẫu nhiên, seed cố định | p95 ≤ 200 ms |
| Phát trong transition | Thời gian mỗi khung khi giải mã 2 clip, chạy `mix` và vẽ | p99 ≤ 33 ms |
| Độ trễ | Từ khi decoder trả khung tới khi present xong | p95 ≤ 50 ms |
| VRAM | `IDXGIAdapter3::QueryVideoMemoryInfo`, cộng `EffectGraph::memory()` để đối chiếu | Đỉnh ≤ 1,5 GB với 3 clip |

Các ngưỡng là đề xuất để có cơ sở quyết định, chưa được user duyệt. Benchmark chạy ở bản release và không bật vsync khi đo phát.

## Sản phẩm bàn giao

- Crate `poc` trong workspace của repo này, chạy được bằng một lệnh.
- Script sinh clip thử và tải FFmpeg vào thư mục bị `.gitignore`.
- File kết quả đo và báo cáo tại `docs/plans/result-poc-phase0-vi.md`, kết luận đạt hay không theo từng chỉ số.

Dừng ở cuối giai đoạn 0 và báo kết quả, chưa sang giai đoạn 1.

## Rủi ro cần kiểm khi làm

- egui-wgpu phải có bản dùng đúng wgpu 30.
- Máy không có clang, nên binding FFmpeg phải sinh bằng libclang đặt riêng ngoài repo rồi commit file đã sinh.
- Hai wgpu device trên một `ID3D12Device` chưa ai thử; `from_d3d12` cũng từ chối runtime D3D12 khác (Agility SDK).
- Lỗi submit làm hỏng `GpuContext` tới hết tiến trình. POC chỉ báo lỗi rồi thoát.
