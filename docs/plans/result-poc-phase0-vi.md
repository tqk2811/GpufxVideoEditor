# Kết quả POC giai đoạn 0

Ngày đo: 2026-11-09. Đặc tả ở [spec-poc-phase0-vi.md](spec-poc-phase0-vi.md). Số liệu thô (lần chạy 8): [result-poc-phase0-raw.json](result-poc-phase0-raw.json).

**Các ngưỡng dưới đây là đề xuất, user chưa duyệt.** Bảng chỉ cho biết kết quả so với đề xuất, chưa phải quyết định đạt hay không của user.

## Máy và lệnh

- NVIDIA GeForce RTX 4060 Laptop GPU, driver 617.42, Intel Core i9-14900HX, Windows 11 Pro 10.0.26300. Máy có thêm Intel UHD Graphics.
- wgpu 30.0.1, egui 0.37, submodule gpufx ở 877149b. FFmpeg `ffmpeg-N-128691-gcc583787bb-win64-lgpl-shared`.
- Clip: `test-clips/clip-a.mp4`, `clip-b.mp4`, `clip-c.mp4` (H.264 1080p30, 20 giây, GOP 30), sinh bằng `scripts/setup-poc.ps1`.

Lệnh đo, chạy tại thư mục gốc repo:

```
cargo build --release -p poc
target\release\poc.exe bench test-clips\clip-a.mp4 test-clips\clip-b.mp4 test-clips\clip-c.mp4 --out <json>
```

Tương đương `cargo run --release -p poc -- bench <clip1> <clip2> <clip3> --out <json>`. Cửa sổ chương trình là cửa sổ thật, kích thước 1280x720 logical; chương trình tự đóng khi đo xong.

## Kết quả so với ngưỡng đề xuất

Ba lần chạy liên tiếp (6, 7, 8) của bản cuối, ghi theo dạng khoảng. Tốc độ tua n = 200 (seed `0x5eed202611090002`, 5 lần đầu bỏ để khởi động), phát n = 590.

| Chỉ số | Cách đo | Đo được | Ngưỡng đề xuất | So với đề xuất |
|---|---|---|---|---|
| Tốc độ tua | Tua cả 2 clip tới vị trí ngẫu nhiên, chạy `mix`, tới khi GPU xong khung ra | p50 36,3-37,2 ms; **p95 55,8-56,6 ms**; p99 60,6-61,8 ms; 0 sai timestamp | p95 ≤ 200 ms | Đạt |
| Phát trong transition | Giải mã 2 clip + `mix` + vẽ egui + present + chờ GPU xong | p50 8,33 ms; p95 8,39-8,40 ms; **p99 8,43-8,44 ms** (đều bị khoá theo tần số màn hình, xem bên dưới) | p99 ≤ 33 ms | Đạt |
| Độ trễ decoder tới present | Từ khi decoder trả khung tới khi `present` trả về | p50 8,16-8,19 ms; **p95 8,23-8,26 ms**; p99 8,27-8,30 ms | p95 ≤ 50 ms | Đạt |
| VRAM đỉnh, 3 clip mở | `IDXGIAdapter3::QueryVideoMemoryInfo` của tiến trình; `graph.memory()` để đối chiếu | **Đỉnh 443 MiB** (nền 170 MiB trước khi mở clip). gpufx: pool 24,9 MB, 1 pass texture + 2 input texture | Đỉnh ≤ 1,5 GB | Đạt |

Phần việc thật của mỗi khung, sau khi trừ thời gian bị chặn ở acquire và present: p50 1,7-2,0 ms, p99 2,5-2,7 ms; riêng tới hết `mix` p99 2,2-2,4 ms. Độ trễ decoder tới GPU xong, trừ phần chặn đó: p95 2,1-2,2 ms.

## Tính đúng của ảnh

Đọc lại texture RGBA ra bộ nhớ và so 2040 điểm với ảnh quy đổi độc lập trên CPU từ NV12 của decoder (BT.601 limited). Kiểm trước và sau toàn bộ tải, cả 2 lần đều đạt:

| progress | Sai số trung bình với blend mong đợi (0-255) | So với clip đầu | So với clip sau |
|---|---|---|---|
| 0 | 0,28 | 0,3 | 126,1 |
| 0,5 | 0,50 | 60,1 | 60,0 |
| 1 | 0,42 | 113,6 | 0,4 |

Độ sáng trung bình 99-123/255, không đen. Ngưỡng tự đặt sai số trung bình ≤ 5.

## Sai khác so với thiết kế

1. Có hai wgpu device trên cùng một `ID3D12Device`: một của app (egui), một của gpufx qua `GpuContext::from_d3d12`. Hai device dùng chung một texture RGBA, không thấy lỗi hay hỏng ảnh trong các lần chạy.
2. Texture đầu vào đi đường sao chép, không zero-copy: decoder cấp phát 1920x1088 cho khung 1920x1080, array size 1, nên gpufx sao chép một lần cho mỗi plane mỗi khung. Khung ra RGBA thì ghi trực tiếp.
3. Đồng bộ bằng chờ CPU: sau mỗi `mix` chờ fence của khung ra, sau mỗi lần vẽ chờ submission của app. Hai queue không chờ nhau trên GPU. Chấp nhận cho POC, chưa phải thiết kế cuối vì chiếm luồng CPU.
4. Tắt vsync chưa thực sự có tác dụng. App chọn `Immediate`, nhưng `present` vẫn chặn 6-7 ms mỗi khung và thời gian khung bị khoá ở 8,33 ms (lần 6-8) hoặc 16,66 ms (lần 3-5, trước đó, cùng ngày). Nguyên nhân chưa kiểm chứng, nghi do đường hiển thị của laptop hai GPU. Các số đo phát và độ trễ ở trên vì vậy phản ánh tần số màn hình; phần việc thật là số đã trừ phần bị chặn.

## Chưa kiểm chứng

- Phát thật không giới hạn tần số: chưa đo được tốc độ hiển thị vượt tần số màn hình.
- Debug layer của D3D12 chưa bật; không có bằng chứng từ validation rằng hai device không xung đột, chỉ có kết quả đúng về ảnh.
- Cửa sổ player mới kiểm bằng chụp ảnh tĩnh (có hình blend, chữ điều khiển đúng). Chưa bấm thử slider và đổi clip bằng tay. Chụp bằng tiến trình không DPI-aware nên ảnh bị cắt phía phải.
- Độ ổn định dài hạn, mất device, các GPU khác, độ phân giải khác, clip có khung đúng 1920x1088.
- Độ trễ tới màn hình thực (photon); phép đo chỉ tới lúc `present` trả về.

## Đề xuất với gpufx

Không sửa `external/FFmpegGpuFilter`. Đề xuất gửi cho phiên giữ repo đó: công khai `GpuContext::from_device` để bỏ device thứ hai (đã gửi trước đây); xem xét đọc tại chỗ vùng khung trong texture lớn hơn khung (`d3d12.rs:365` yêu cầu đúng kích thước) để bỏ bước sao chép.
