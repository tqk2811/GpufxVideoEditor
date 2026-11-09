# Backlog

### [Vừa] Cần bản FFmpeg shared GPLv3 cho win64

Editor đã chọn FFmpeg GPLv3 để xuất bằng x264. Release `ffmpeg-N-128691-gcc583787bb` của tqk2811/FFmpegGpuFilter chỉ có bản LGPL. Cần asset win64 biến thể `gpl` cùng commit cc583787bb, có header và thư viện. User đồng ý tạo release ngày 2026-11-09, phiên FFmpegGpuFilter đã đóng nên yêu cầu chưa gửi được. Chặn bước test xuất x264 của `docs/plans/spec-engine-phase1-vi.md`. Ghi ngày 2026-11-09.

### [Vừa] Tua clip GOP dài vượt ngưỡng đề xuất

Clip H.264 GOP 250 có B-frame tua p95 502 ms so với ngưỡng đề xuất 200 ms (`docs/plans/result-poc-phase0-vi.md`). Footage thật thường có GOP dài, nên giai đoạn sau cần proxy, cache hoặc giải mã trước. Ghi ngày 2026-11-09.

### [Vừa] Chờ decoder rảnh trước mỗi lần mix làm mất giải mã song song

`D3d12Decoder::wait_until_idle` ở `crates/poc/src/decoder.rs` và `SequentialDecoder::wait_idle` ở `crates/d3d12-backend/src/decoder.rs:389-401` chờ CPU trước mỗi `mix` để khung không còn bị decode khác tham chiếu. Phát p99 có lần lên 31,4 ms, sát ngưỡng 33 ms, chưa rõ nguyên nhân. Bản editor cần đồng bộ trên GPU hoặc tách luồng. Ghi ngày 2026-11-09.

### [Thấp] Fence theo khung đã cấp phát, chưa theo khung đã submit

`crates/poc/src/decoder.rs:52-58` và `crates/d3d12-backend/src/decoder.rs:36-44` theo dõi khung qua `get_buffer2`. Nếu cấp phát xong mà không submit (slice hỏng), fence mới nhất có thể không phủ các decode trước đó. Chưa gặp với clip H.264 đã thử; điều này suy từ cách FFmpeg hoạt động, chưa kiểm chứng. Ghi ngày 2026-11-09.

### [Thấp] Chưa cô lập nguồn lỗi validation #1334

Debug layer báo #1334 (CopyTextureRegion trên COPY list) 24 lần. Số lần khớp bước đọc ngược pixel `av_hwframe_transfer_data` trong kiểm tra, nhưng chưa cô lập (`POC_D3D12_DEBUG=1`). Ghi ngày 2026-11-09.

### [Thấp] Benchmark POC chưa kiểm hết tính đúng của ảnh

Blend của `crates/poc/src/mix_bench.rs` chỉ kiểm ở 3 vị trí tua, ma trận màu lấy cùng nguồn với gpufx, khung lúc phát chưa kiểm, player chưa thao tác bằng tay. Bộ test tự động của engine đã có (`crates/gpufx-render/tests/`), nhưng không có test nào chứng minh được việc chờ decoder (`wait_idle`) thật sự chặn đúng lỗi tham chiếu đồng thời: bỏ nó đi thì test có thể vẫn xanh, vì lỗi loại này không lộ thành sai ảnh ổn định. Ghi ngày 2026-11-09.

### [Vừa] Encoder phần cứng h264_d3d12va chưa dùng được

`crates/d3d12-backend/src/encoder.rs` chỉ gửi khung NV12 CPU. `h264_d3d12va` đòi `AV_PIX_FMT_D3D12`, nên hiện báo lỗi `Invalid argument`; `libopenh264` đòi `yuv420p` nên cũng không mở được. Hai encoder này có trong FFmpeg LGPL; muốn dùng cần upload khung lên hw frames context và thêm nhánh `yuv420p`. Ghi ngày 2026-11-09.

### [Vừa] Bộ giải mã chưa xử lý nguồn đổi kích thước giữa chừng và nguồn 10-bit

`crates/d3d12-backend/src/decoder.rs` từ chối mọi nguồn không phải NV12 8-bit (P010/10-bit). Kích thước khung coi như không đổi suốt một clip; nguồn đổi độ phân giải giữa file chưa có bài test và chưa xử lý. Ghi ngày 2026-11-09.

### [Thấp] Đưa clip về kích thước project đang kéo giãn, downscale chỉ bilinear

Package `scale` của `crates/d3d12-backend/src/compositor.rs` kéo giãn thẳng theo kích thước project, không giữ tỉ lệ và không có bộ lọc chất lượng cao hơn bilinear khi thu nhỏ mạnh (4K về 1080p). Cần chốt cách fit/fill ở giai đoạn sau và xem xét mipmap hoặc bicubic. Ghi ngày 2026-11-09.

### [Thấp] Layer dưới cùng chưa ghép lên nền đen

`crates/engine/src/render.rs` dùng thẳng ảnh của layer dưới cùng làm canvas. Nếu effect của layer đó tạo alpha nhỏ hơn 1 thì kết quả giữ alpha đó (encoder bỏ alpha). Hiện mọi nguồn là opaque nên chưa lộ. Ghi ngày 2026-11-09.

### [Thấp] Hiệu năng render còn chờ CPU theo từng khung

`D3d12Compositor::read` đọc ngược mỗi khung và chờ CPU, hash SHA-256 và NV12 chạy trên CPU; 1080p bản debug khoảng 6 khung/giây. Chưa đo bản release, chưa dùng đồng bộ GPU cho đường encode. Ghi ngày 2026-11-09.

### [Vừa] Demuxer open-GOP hoặc seek theo DTS có thể trả khung đầu sau seek muộn hơn đích

`crates/d3d12-backend/src/decoder.rs:348-352` và `:365` nhận khung đầu tiên sau seek dù pts của nó lớn hơn đích, coi như nguồn bắt đầu ở đó. Với demuxer seek theo DTS hoặc luồng open-GOP, khung đầu sau seek có thể muộn hơn đích trong khi khung đúng nằm trước nó, và lỗi này bị bỏ qua âm thầm. Chưa kiểm chứng; các test hiện chỉ dùng MP4 closed-GOP. Ghi ngày 2026-11-09.

### [Vừa] Đầu ra 23,976 và 59,94 fps không biểu diễn được bằng frame_duration nguyên

Đơn vị thời gian 1/90000 cho 3753,75 tick mỗi khung ở 23,976 fps và 1501,5 ở 59,94 fps (`docs/plans/spec-engine-phase1-vi.md:24`, `crates/engine/src/time.rs`). `frame_duration` là số nguyên nên hai nhịp này bị làm tròn và lệch dần. Cần đổi đơn vị thời gian hoặc lưu nhịp khung dạng phân số. Ghi ngày 2026-11-09.

### [Vừa] Hai đường màu độc lập: shader gpufx và hàm rgba_to_nv12 viết tay

Giải mã và ghép dùng shader gpufx, còn xuất dùng `rgba_to_nv12` ở `crates/d3d12-backend/src/encoder.rs:185-207` với hệ số nguyên tự viết, nên sai lệch giữa hai đường không có gì kiểm (lỗi hệ số Cb từng nằm ở đây). Nên cân nhắc swscale hoặc cho gpufx xuất thẳng NV12. Kèm vấn đề vị trí mẫu chroma: trung bình 2x2 là đặt chroma ở tâm khối, trong khi H.264 thường là left-sited, và encoder không đặt `chroma_sample_location`. Ghi ngày 2026-11-09.

### [Thấp] Khoảng trống của bộ test render

`the_decoder_seeks_only_to_go_backward` không đếm số lần seek (`crates/gpufx-render/tests/pipeline.rs:226-241`). Ngưỡng 16 của test chuyển cảnh (`crates/gpufx-render/tests/pipeline.rs:161`) che việc lấy nhầm clip `to` ở tiến độ thấp. Chưa có test cho: giữ khung đầu, màu, `start_time` khác 0, đường dẫn tương đối ở CLI, mẫu giữa GOP. Ghi ngày 2026-11-09.

### [Thấp] Dọn dẹp rải rác ở engine và backend

`canvas` không được giải phóng trên đường lỗi (`crates/engine/src/render.rs:264-282`). `clip_graphs` không bao giờ bị loại bỏ (`crates/d3d12-backend/src/compositor.rs:186`), nên project nhiều clip giữ graph của mọi clip. Thư mục package mặc định là đường dẫn lúc build (`crates/gpufx-render/src/main.rs:51`), không chạy được khi file thực thi được chép đi. `Decoder::wait_idle` trong trait trung lập là khái niệm riêng của D3D12VA. Ghi ngày 2026-11-09.
