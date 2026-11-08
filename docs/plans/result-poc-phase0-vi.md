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

Ba lần chạy liên tiếp (6, 7, 8), ghi theo dạng khoảng, đo trước bản sửa ở mục 5 phần sai khác (số sau bản sửa ở mục riêng bên dưới). Chỉ JSON của lần 8 được commit; số của lần 6 và 7 chỉ còn trong bảng này. Tốc độ tua n = 200 (seed `0x5eed202611090002`, 5 lần đầu bỏ để khởi động), phát n = 590.

| Chỉ số | Cách đo | Đo được | Ngưỡng đề xuất | So với đề xuất |
|---|---|---|---|---|
| Tốc độ tua | Tua cả 2 clip tới vị trí ngẫu nhiên, chạy `mix`, tới khi GPU xong khung ra | p50 36,3-37,2 ms; **p95 55,8-56,6 ms**; p99 60,6-61,8 ms; 0 sai timestamp | p95 ≤ 200 ms | Đạt |
| Phát trong transition | Giải mã 2 clip + `mix` + vẽ egui + present + chờ GPU xong | p50 8,33 ms; p95 8,39-8,40 ms; **p99 8,43-8,44 ms** (nghi bị giới hạn nhịp hiển thị, đây là suy luận, xem mục 4 phần sai khác) | p99 ≤ 33 ms | Đạt |
| Độ trễ decoder tới present | Từ khi decoder trả khung tới khi `present` trả về | p50 8,16-8,19 ms; **p95 8,23-8,26 ms**; p99 8,27-8,30 ms (cùng lưu ý về nhịp hiển thị) | p95 ≤ 50 ms | Đạt |
| VRAM đỉnh, 3 clip mở | `IDXGIAdapter3::QueryVideoMemoryInfo` của tiến trình; `graph.memory()` để đối chiếu | **Đỉnh 443 MiB** (nền 170 MiB trước khi mở clip). gpufx: pool 24,9 MB, 1 pass texture + 2 input texture | Đỉnh ≤ 1,5 GB | Đạt |

Kết quả "Đạt" của phát và độ trễ chỉ có giá trị như cận trên: giới hạn nhịp chỉ có thể làm các số này xấu đi, nên số đo vẫn nằm dưới ngưỡng. Ngược lại, p50 8,33 ms không nói gì về năng lực xử lý. Điều kiện tắt vsync của đặc tả chưa được đáp ứng (mục 4 phần sai khác).

Cận dưới của phần việc mỗi khung, sau khi trừ thời gian bị chặn ở acquire và present: p50 1,7-2,0 ms, p99 2,5-2,7 ms; riêng tới hết `mix` p99 2,2-2,4 ms. Độ trễ decoder tới GPU xong, trừ phần chặn đó: p95 2,1-2,2 ms. Trong lúc bị chặn, GPU vẫn vẽ xong và decoder vẫn giải mã trước, nên đây không phải chi phí khi chạy không giới hạn nhịp và không được quy đổi thành FPS.

## Sau bản sửa chờ decoder (mục 5 phần sai khác)

Ba lần chạy bình thường trên cùng clip GOP 30, cùng seed và cùng n, ghi theo dạng khoảng:

| Chỉ số | Đo được |
|---|---|
| Tốc độ tua | p50 38,1-41,5 ms; p95 58,2-66,0 ms; p99 63,9-74,8 ms; 0 sai timestamp |
| Phát trong transition | p50 8,33-15,66 ms; p95 8,38-30,56 ms; p99 8,51-31,42 ms |
| Độ trễ decoder tới present | p50 8,17-15,44 ms; p95 8,24-30,40 ms; p99 8,31-31,22 ms |
| VRAM đỉnh | 442 MiB |
| Cận dưới phần việc mỗi khung | p50 5,90-8,46 ms; p99 7,10-13,50 ms |

So với ngưỡng đề xuất, cả bốn chỉ số vẫn đạt, nhưng p99 phát 31,42 ms đã sát ngưỡng 33 ms. Nhịp khung dao động giữa các lần (hai lần đầu p95 khoảng 30 ms, lần thứ ba 8,38 ms), chưa rõ nguyên nhân. Cận dưới phần việc tăng từ khoảng 1,7 ms lên 5,9-8,5 ms vì CPU nay chờ decoder xong trước mỗi `mix`, đúng phần việc mà số cũ không tính.

## Tua trên clip GOP dài

Clip `test-clips/gop250-a.mp4`, `gop250-b.mp4`, `gop250-c.mp4` (nội dung như clip GOP 30, H.264 1080p30, 20 giây, GOP 250, B-frame 2, không đổi khung khoá theo cảnh), sinh bằng ffmpeg trên PATH, không commit. Khung khoá của clip a nằm ở 0; 8,33; 16,67 giây (ffprobe). Một lần chạy, bản sau sửa, cùng định nghĩa tua và cùng seed:

| n | p50 | p95 | p99 | max | Sai timestamp |
|---|---|---|---|---|---|
| 200 | 278,6 ms | 502,3 ms | 544,8 ms | 573,9 ms | 0 |

Bảng chính vẫn là clip GOP 30. Với GOP 250, p95 502,3 ms vượt ngưỡng đề xuất 200 ms; tua chính xác tốn thời gian tỉ lệ với số khung phải giải mã từ khung khoá gần nhất, và mỗi lần đo tua cả hai clip.

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
4. Tắt vsync chưa thực sự có tác dụng, điều kiện không vsync của đặc tả chưa được đáp ứng. App chọn `Immediate`, nhưng `present` vẫn chặn 6-7 ms mỗi khung và thời gian khung tập trung ở 8,33 ms (lần 6-8) hoặc 16,66 ms (lần 3-5, trước đó, cùng ngày). "Khoá theo tần số màn hình" chỉ là suy luận: JSON không ghi tần số, và chưa giải thích được vì sao lúc 16,66 ms lúc 8,33 ms. Tần số đọc được sau đó từ `Win32_VideoController` (không phải lúc đo): NVIDIA 240 Hz ở 2560x1600, Intel 60 Hz ở 1920x1080; 8,33 ms ứng với 120 Hz, không khớp số nào trong đó. Nghi do đường hiển thị của laptop hai GPU, chưa kiểm chứng. Các số phát và độ trễ vì vậy là cận trên; số đã trừ phần bị chặn là cận dưới, không đại diện cho chi phí khi không giới hạn nhịp.
5. Trước mỗi `mix`, app chờ mọi lần giải mã đã gửi của các decoder liên quan xong. FFmpeg gửi lệnh giải mã rồi trả về ngay, và giữ các khung tham chiếu ở `VIDEO_DECODE_READ` trong lúc đó (`ff_d3d12va_common_end_frame`). Với B-frame, một khung được trả ra sau khi các khung tham chiếu nó đã được gửi đi giải mã, nên fence riêng của khung không đảm bảo điều `process_d3d12` yêu cầu (`d3d12.rs:462-464`). Đo được 948/951 lần `mix` có lệnh giải mã còn chạy khi bắt đầu chờ. Bộ đếm này chứng minh decoder còn bận, không chứng minh từng lần đều đụng đúng khung đó. Bản sửa chờ trên CPU, làm mất phần giải mã trước song song với `mix`.

## Chưa kiểm chứng

- Phát thật không giới hạn tần số: chưa đo được tốc độ hiển thị vượt tần số màn hình.
- Đã bật debug layer và GPU-based validation (`POC_D3D12_DEBUG=1`), một lần chạy `bench` trước và một lần sau bản sửa, kết quả giống nhau: lỗi #1334 × 24, cảnh báo #820 × 4, #1361 × 2, #1424 × 1, thông báo #1016 × 1, không mất thông báo nào. #1334 là `CopyTextureRegion` trên command list COPY với nguồn ở COPY_SOURCE; số 24 khớp đúng 6 lần kiểm ảnh × 2 khung × 2 plane của `av_hwframe_transfer_data`, tức nhiều khả năng từ đường đọc lại của FFmpeg dùng để kiểm chứng, không phải từ `mix` (suy từ phép đếm, chưa cô lập riêng). Không có lỗi nào về trạng thái texture khung giải mã khi gpufx dùng, hay về dùng chung tài nguyên giữa hai wgpu device. Nhưng lần chạy trước bản sửa cũng không báo gì về việc tham chiếu đồng thời nêu ở mục 5, nên validation không phát hiện được loại lỗi này giữa queue video và queue khác; không có thông báo không chứng minh là không có xung đột. Số đo của các lần chạy debug không dùng làm số benchmark.
- Kiểm blend chỉ chứng minh trộn đúng ở 3 vị trí tua. Ma trận của phép kiểm lấy từ chính `color_info` đưa cho gpufx, còn range viết cứng là limited, nên nếu colorspace sai thì phép kiểm không phát hiện được.
- Khung lúc phát (playback) không được kiểm chứng về ảnh.
- Tua qua `mix` chỉ kiểm timestamp; kiểm số khung in trong ảnh mới làm trên một clip, ở `decode-bench`, không qua `mix`.
- Số tua đo trên clip tổng hợp với file nằm trong cache của OS; footage thật có thể khác.
- Cửa sổ player mới kiểm bằng chụp ảnh tĩnh (có hình blend, chữ điều khiển đúng). Chưa bấm thử slider và đổi clip bằng tay. Chụp bằng tiến trình không DPI-aware nên ảnh bị cắt phía phải.
- Độ ổn định dài hạn, mất device, các GPU khác, độ phân giải khác, clip có khung đúng 1920x1088.
- Độ trễ tới màn hình thực (photon); phép đo chỉ tới lúc `present` trả về.

## Đề xuất với gpufx

Không sửa `external/FFmpegGpuFilter`. Đề xuất gửi cho phiên giữ repo đó: công khai `GpuContext::from_device` để bỏ device thứ hai (đã gửi trước đây); xem xét đọc tại chỗ vùng khung trong texture lớn hơn khung (`d3d12.rs:365` yêu cầu đúng kích thước) để bỏ bước sao chép.
