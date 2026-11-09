# Backlog

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

### [Vừa] Transition giữa hai clip không có thêm tiếng ngoài điểm in/out không phải crossfade thật

Trong cửa sổ transition, mỗi clip được kéo dài qua đầu hoặc đuôi của chính nó, nhưng khi nguồn không còn tiếng ngoài điểm in/out thì phần đầu và đuôi nằm ngoài nguồn là im lặng, trong khi hình giữ khung đầu hoặc khung cuối. Trường hợp thường gặp: nối hai file nguyên (`crates/engine/src/audio_schedule.rs:163-164`, `crates/engine/src/audio_mix.rs`). Nên làm vì tiếng tắt dần rồi bật lên thay vì hoà vào nhau. Ghi ngày 2026-11-09.

### [Vừa] Track audio dài hơn khung hình video cuối bị cắt, project chỉ có track audio ra 0 mẫu

Độ dài tiếng theo số khung hình video (`audio_sample_count` trong `crates/engine/src/audio_schedule.rs`), nên phần track audio vượt quá khung hình cuối bị cắt, và project chỉ có track audio có 0 mẫu. Nên làm vì nhạc nền dài hơn video bị mất đuôi và không xuất được project chỉ có tiếng. Ghi ngày 2026-11-09.

### [Thấp] Hai ghi chú về chất lượng tiếng

Clip liên tục bị cắt tại tick không chia hết cho 15 thì lặp hoặc rớt tối đa 1 mẫu ở chỗ nối, có thể gây tiếng tách. Crossfade công suất đều của tiếng tương quan (hai nửa của một clip bị cắt) đạt đỉnh khoảng +3 dB và có thể bị chặn ở mức tối đa. Ghi ngày 2026-11-09.

### [Thấp] Downmix 5.1 không chuẩn hoá, và hai nhánh decoder chưa có test

Resampler ở `crates/d3d12-backend/src/audio.rs:430` xuất float và không chuẩn hoá, nên downmix 5.1 có thể vượt 1.0 và bị tính là clamp khi mix. Chưa đo trên nguồn 5.1 thật. Nhánh lấp khoảng trống timestamp (`crates/d3d12-backend/src/audio.rs:406`, mới có unit test cho giới hạn) và nhánh đổi định dạng giữa dòng (`crates/d3d12-backend/src/audio.rs:393-397`) chưa có test trên file thật. Nên làm vì âm có thể méo mà không có gì báo, và hai nhánh này khó tái hiện khi có lỗi. Ghi ngày 2026-11-09.

### [Thấp] AAC trong Matroska vẫn mất priming khi seek về đầu

Seek về đầu của AAC trong MKV vẫn sai khoảng 1024 mẫu đầu (đo được sai 0,23 so với `ffmpeg` giải mã không seek), dù AAC trong MP4 đã đúng sau bản sửa `seek_demuxer` ở `crates/d3d12-backend/src/audio.rs`. Gói priming có timestamp -21 ms; nhiều khả năng demuxer Matroska kẹp timestamp seek về mục cue đầu rồi bỏ qua gói trước đó, chưa kiểm chứng bằng log. Nên làm nếu nguồn MKV là chuyện thường gặp, vì đầu clip nghe bị rè. Ghi ngày 2026-11-09.

### [Thấp] Kéo clip tới mép timeline không tự cuộn

Khi kéo clip hoặc cạnh trim tới mép trái/phải của vùng track, view đứng yên nên không kéo được tới phần đang ngoài màn hình; phải thả, cuộn rồi kéo lại (`crates/gpufx-editor/src/ui.rs`, `handle_tracks`). Ghi ngày 2026-11-09.

### [Thấp] Vị trí thả clip chưa làm tròn theo frame_duration

Thời điểm thả và trim lấy theo điểm ảnh rồi đổi ra tick, không làm tròn về bội của `frame_duration` (`crates/gpufx-editor/src/timeline/drag.rs`, `drag_command`). Clip có thể bắt đầu giữa hai khung. Cần quyết định trước khi xuất video. Ghi ngày 2026-11-09.

### [Thấp] POC duyệt TexturesDelta không drain

`crates/poc/src/presenter.rs:132` duyệt `TexturesDelta` mà không `drain`. Cùng kiểu mã này đã panic ("Dropped TexturesDelta with unapplied deltas") ở bản debug của editor; chưa xác nhận trong POC. Ghi ngày 2026-11-09.

### [Thấp] Khung đầu tiên trên device mới vẫn ra trễ khoảng 1 giây vì dựng pipeline

`external/FFmpegGpuFilter/crates/gpufx-core/src/convert.rs:354` dựng pipeline chuyển đổi khung GPU (`to_rgba_gpu`) ở lần dùng đầu của mỗi `GpuContext`, mất khoảng 970 ms (đo 2026-11-09). Việc này đã nằm ngoài cổng submit nên UI vẫn present được (cổng giữ tối đa khoảng 13 ms), nhưng khung đầu tiên vẫn hiện sau khoảng 1,1 giây. Nên làm nếu muốn hình đầu ra nhanh hơn: dựng sẵn lúc khởi tạo device ở luồng nền, hoặc cache pipeline. Ghi ngày 2026-11-09.

### [Thấp] Lỗi wgpu của luồng UI chỉ hiện chớp qua ở dòng preview

`crates/d3d12-backend/src/gpu.rs` giờ giữ lỗi theo từng luồng (wgpu gọi handler trên luồng gây lỗi), nên lỗi của luồng UI không còn làm hỏng frame hay export. `crates/gpufx-editor/src/window.rs` (`collect_preview`) chỉ đưa lỗi của luồng UI vào `preview_error`, và frame preview kế tiếp ghi đè nó. Nên làm vì lỗi UI thật có thể trôi mất; nên có chỗ hiển thị riêng. Ghi ngày 2026-11-09.

### [Thấp] `clip_graphs` và `transition_graphs` không bao giờ được dọn

`crates/d3d12-backend/src/compositor.rs:284-285`: hai cache này chỉ thêm, nên preview chạy lâu hoặc project nhiều clip giữ graph của mọi clip và mọi transition đã gặp. Nên làm vì bộ nhớ GPU tăng theo số clip đã xem. Ghi ngày 2026-11-09.

### [Thấp] Tua qua ranh giới clip đóng decoder

`crates/engine/src/render.rs:342`: `retain` đóng decoder của clip không dùng ở frame hiện tại, nên tua qua lại giữa hai clip phải mở lại file và decode lại từ keyframe. Nên làm vì scrub qua điểm nối bị khựng. Ghi ngày 2026-11-09.

### [Thấp] Thiết bị không phải 48 kHz F32 bị từ chối, chưa có resample

`crates/gpufx-editor/src/playback/plan.rs`, `choose_format`: thiết bị không có 48 kHz float thì không phát tiếng, chỉ chạy đồng hồ bằng timer và báo lý do ở thanh trạng thái. Nên làm vì máy dùng mix format 44,1 kHz hoặc 24-bit sẽ không nghe được gì. Ghi ngày 2026-11-09.

### [Thấp] Mở thiết bị âm thanh trên luồng UI làm khựng lần bấm Space đầu tiên

`crates/gpufx-editor/src/window.rs`, `apply_transport`: `Playback::open` chạy ngay trên luồng UI, đo được 100 đến 330 ms (2026-11-09). `cpal::Stream` của WASAPI là `Send` nên mở được trên luồng nền như các dialog. Nên làm vì lần bấm Space đầu tiên làm UI đứng trong khoảng đó. Ghi ngày 2026-11-09.

### [Thấp] Đổi thiết bị mặc định làm dừng phát kèm lỗi, kể cả lúc đang rảnh

`crates/gpufx-editor/src/playback/device.rs`, error callback của stream: thiết bị mặc định đổi thì stream báo lỗi, `Playback` bị bỏ và thanh trạng thái hiện lỗi dù đang không phát. Nên làm vì cắm tai nghe là thao tác thường ngày, không phải lỗi. Ghi ngày 2026-11-09.

### [Thấp] Seek không có prefill, decoder nguội có thể gây giật

`crates/gpufx-editor/src/playback/thread.rs`, `PreparingThread::seek`: gate vẫn mở khi seek, nên decoder nguội phải đuổi kịp trong lúc đang phát. Bài `measure_silence_with_a_slow_source` chỉ đo bằng nguồn giả; cần nghe thử trên media thật để quyết định có cần đóng gate tới khi đủ lead không. Ghi ngày 2026-11-09.

### [Thấp] Project thiếu file nguồn hoặc thiếu package không mở được

`crates/gpufx-editor/src/project_file.rs`, `parse_project`: mở project chạy `validate` đầy đủ nên một file nguồn bị di chuyển làm cả project bị từ chối, người dùng không có cách trỏ lại đường dẫn. Nên làm vì media hay bị đổi thư mục; cần hộp thoại định vị lại file khi mở. Ghi ngày 2026-11-09.

### [Thấp] Quay undo về đúng trạng thái đã lưu vẫn tính là chưa lưu

`crates/gpufx-editor/src/editor.rs`, `is_dirty` so số revision, mà undo cũng tăng revision, nên undo về đúng bản đã lưu vẫn hỏi lưu khi đóng. Nên làm nếu cảnh báo thừa gây khó chịu. Ghi ngày 2026-11-09.

### [Vừa] Export chạy trong tiến trình editor

`crates/gpufx-editor/src/export.rs` chạy render ngay trong tiến trình editor (luồng `export`) trên cùng device với preview, nên device bị mất lúc export kéo sập cả editor, và preview phải tạm dừng suốt thời gian export. User đã chốt làm: chạy `gpufx-render` thành tiến trình con và lấy tiến độ qua stdout, vì sau này có bản chạy không giao diện cho agent MCP. Ghi ngày 2026-11-09.

### [Thấp] Seek trong lúc clone project bị kẹp về điểm cuối cũ khi timeline dài ra

`crates/gpufx-editor/src/playback/thread.rs:241`: lệnh seek tới trong lúc `producer.set_project` đang clone bị kẹp theo điểm cuối của project cũ, nên timeline vừa dài ra thì seek quá điểm cuối cũ bị đưa về đó. Nên làm vì người dùng sẽ thấy tua sai vị trí đúng lúc vừa sửa. Ghi ngày 2026-11-09.

### [Thấp] Chế độ timer không có thiết bị âm thanh không bao giờ thử lại

`crates/gpufx-editor/src/playback/device.rs:37`: khi không mở được thiết bị thì clock chạy theo timer, và thiết bị không được thử lại cho tới lần mở lại `Playback`. Sửa kèm chú thích "stays until a device opens" ở `crates/gpufx-editor/src/editor.rs` cho đúng hành vi. Nên làm vì cắm tai nghe vào giữa chừng vẫn không có tiếng. Ghi ngày 2026-11-09.

### [Thấp] Bản render giữ lại khi đổi tên thất bại không được dọn

`crates/d3d12-backend/src/export.rs:125`: file đích bị khoá thì mỗi lần xuất lại để thêm một file `*.tmp.*` đầy đủ, không có gì dọn. Nếu `out` đổi tên xong mà file hash lỗi thì báo thất bại dù đích đã bị thay. Nên thử lại hoặc kiểm quyền ghi trước khi render. Ghi ngày 2026-11-09.

### [Thấp] Video không ghi độ dài mượn độ dài của container

`crates/d3d12-backend/src/probe.rs:66`: luồng video không ghi độ dài sẽ lấy độ dài container. Với mkv có video 1 s và audio 3 s thì container là 3 s, nên 2 s cuối clip giữ nguyên khung cuối. Ghi ngày 2026-11-09.

### [Thấp] Probe không có interrupt callback

`crates/d3d12-backend/src/probe.rs`: `avformat_open_input` và `avformat_find_stream_info` không có interrupt callback. Ổ mạng chết làm một lượt nạp chạy mãi và chặn các yêu cầu khác cho tới khi đóng app. Nên cân nhắc một chỗ chạy riêng cho lượt nạp và hộp thoại, tách khỏi nhau. Ghi ngày 2026-11-09.

### [Thấp] Khung UI có thể đói khi hiệu ứng nặng giữ cổng submit

`crates/gpufx-editor/src/presenter.rs`: UI chỉ thăm dò `try_lock_submits` mỗi 8 ms, nên khi một hiệu ứng nặng giữ cổng liên tục nó có thể không bao giờ lọt vào; các cú nhấp xếp hàng cho tới khi lọt. Ghi ngày 2026-11-09.


### [Vừa] Hiệu ứng làm đổi kích thước ảnh đầu ra bị từ chối ngay khung đầu

`crates/d3d12-backend/src/compositor.rs`: hiệu ứng như `scale` với `w`, `h` khác kích thước project (160x90 trong ảnh 320x180) bị gpufx từ chối, kể cả sau khi dựng lại graph, nên preview báo lỗi và hiệu ứng này chưa dùng được. Cần quyết định co ảnh về kích thước project hay ẩn các tham số đó. Ghi ngày 2026-11-09.

### [Thấp] Nhánh dựng lại graph của transition chưa có test

`crates/d3d12-backend/src/compositor.rs` (`needs_rebuild`): chưa có gói transition nào dùng size function, nên test GPU chỉ phủ đường cập nhật tại chỗ. Nhánh dựng lại dùng chung logic với clip effect. Ghi ngày 2026-11-09.

### [Thấp] Tìm trong thư viện phân biệt dạng Unicode và dấu

`crates/gpufx-editor/src/media.rs:79-82`: tên NFC và NFD không khớp nhau, gõ "dep" không ra "đẹp". Ghi ngày 2026-11-09.

### [Thấp] Thư viện không đánh dấu file đã mất và không có nút gỡ

`crates/gpufx-editor/src/media.rs`, `crates/gpufx-editor/src/ui/left_panel.rs`: file bị xoá khỏi đĩa vẫn còn trong lưới, bấm vẫn thêm clip, lỗi chỉ lộ ở preview hoặc khi xuất. Ghi ngày 2026-11-09.

### [Thấp] Nhập video từ tab Âm thanh thì mục nằm ở tab Phương tiện

`crates/gpufx-editor/src/media.rs:36-42`: `audio_only` chỉ lọc hộp thoại, người dùng ở tab Âm thanh không thấy mục vừa nhập. Ghi ngày 2026-11-09.

### [Thấp] Lưới thư viện dựng lại toàn bộ mỗi khung

`crates/gpufx-editor/src/ui/left_panel.rs:71,83`: clone danh sách và `to_lowercase` mỗi khung, lưới chưa dùng `show_rows`. Thư viện vài trăm file sẽ tốn thời gian khung. Ghi ngày 2026-11-09.

### [Thấp] Đường dẫn cài đặt chỉ chạy trên Windows

`crates/gpufx-editor/src/settings.rs`, `settings_file`: lấy thư mục từ biến `APPDATA`. Khi làm bản Linux phải có đường dẫn cài đặt riêng (`XDG_CONFIG_HOME`), nếu không app không lưu được cài đặt. Ghi ngày 2026-11-09.

### [Thấp] Dữ liệu mức âm thanh chưa có cache trên đĩa

`crates/gpufx-editor/src/visuals/mod.rs:1-8`: cache mức âm chỉ nằm trong bộ nhớ, nên mỗi lần mở lại project phải đọc lại toàn bộ tiếng của từng file. Nên làm vì file dài tốn nhiều phút mỗi phiên. Ghi ngày 2026-11-09.

### [Thấp] Thumbnail bỏ qua thông tin màu và alpha của nguồn

`crates/d3d12-backend/src/thumbnail.rs:265` (`convert_kept`): `sws_getContext` dùng hệ màu mặc định, chưa gọi `sws_setColorspaceDetails` theo colorspace/range của khung, và nguồn có alpha chưa được xử lý riêng. Nên làm vì nguồn BT.709 hoặc full range có thumbnail lệch màu so với preview. Ghi ngày 2026-11-09.

### [Thấp] Metadata xoay video chưa được áp dụng ở thumbnail và preview

`crates/d3d12-backend/src/thumbnail.rs:102` (`initialize`) và đường decode preview: display matrix của luồng video không được đọc. Nên làm vì video quay dọc từ điện thoại hiện sai chiều. Ghi ngày 2026-11-09.

### [Thấp] Worker thumbnail có thể kẹt trên ổ mạng chết

`crates/d3d12-backend/src/thumbnail.rs:102` và `crates/d3d12-backend/src/audio.rs:168` (`avformat_open_input`): không có interrupt callback nên worker kẹt tới khi đóng app, chỉ `cancelled` giữa các gói là dừng được. Nên làm vì một ổ mạng chết chiếm hết worker và thumbnail của mọi file khác đứng lại. Ghi ngày 2026-11-09.

### [Thấp] Nguồn đang đọc mức âm chặn nguồn hiện trên màn hình

`crates/gpufx-editor/src/visuals/worker.rs:255` (`run_worker`, nhánh `Lane::Audio`): chỉ có một worker audio và job đã chạy không nhường chỗ, nên file dài đã cuộn khỏi màn hình làm chậm bars của file đang nhìn. Muốn nhường phải huỷ rồi đọc lại từ đầu và theo dõi nguồn nào đang được vẽ, không đơn giản nên chưa làm. Ghi ngày 2026-11-09.

### [Thấp] Hiện lại track ẩn có file thiếu rồi lưu thì project mở lên bị SourceNotFound

`crates/engine/src/edit.rs:128-129` (`AcceptAll`) và `crates/gpufx-editor/src/project_file.rs:99,119`: track ẩn được bỏ qua khi kiểm file nguồn, nên bật hiện lại track có file đã mất vẫn được chấp nhận. Lưu xong, lần mở sau project bị từ chối với SourceNotFound. Cần quyết định: cảnh báo khi bật hiện, hoặc cho mở project và tự ẩn track có file thiếu. Ghi ngày 2026-11-09.

### [Thấp] Mất ổ dưới 5 giây hoặc trong lúc phát để lại Tile::Failed

`crates/gpufx-editor/src/visuals/mod.rs:38,377-383`: ổ bị rút ngắn (dưới 5 giây) hoặc mất đúng lúc đang phát thì ô thumbnail/mức âm chuyển sang `Tile::Failed` và không được thử lại. Nên thử lại như trường hợp mất ổ lâu hơn. Ghi ngày 2026-11-09.

### [Thấp] Đọc peaks ở chế độ tạm dừng giữ file mở trong lúc phát hoặc xuất

`crates/gpufx-editor/src/visuals/worker.rs:338-348`: lượt đọc peaks đã bắt đầu lúc tạm dừng vẫn giữ file nguồn mở khi người dùng bấm phát hoặc xuất, nên có thể tranh chấp tài nguyên với decoder. Nên huỷ hoặc nhả file khi phát hoặc xuất bắt đầu. Ghi ngày 2026-11-09.

### [Thấp] Bản dựng cũ âm thầm bỏ `hidden`/`locked` và xuất cả track bị ẩn

`crates/engine/src/project.rs:74,78` (`locked`, `hidden`): phiên bản project vẫn là 2 và `Track` không có `deny_unknown_fields`, nên bản dựng cũ mở project mới sẽ bỏ qua hai trường này không báo gì, rồi xuất luôn track đã ẩn. Nên tăng phiên bản project hoặc cho bản cũ từ chối trường lạ. Ghi ngày 2026-11-09.

### [Thấp] `window.fullscreen()` đọc cờ ý định của winit, không phải trạng thái của hệ điều hành

`crates/gpufx-editor/src/window.rs:456`: `window.fullscreen()` trả về chế độ winit được yêu cầu chứ không phải trạng thái thật của cửa sổ, nên Win+mũi tên hoặc đổi màn hình có thể để editor tin là vẫn fullscreen và các panel vẫn ẩn. Cần thử tay trên máy thật rồi mới quyết định có theo dõi trạng thái hệ điều hành hay không. Ghi ngày 2026-11-09.

