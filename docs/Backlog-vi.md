# Backlog

### [Vừa] Giải mã ảnh không huỷ được và ảnh hỏng được giải mã lại mỗi lần render

`crates/d3d12-backend/src/compositor.rs` `load_still` (khối gọi `read_still` và `Image::upload`) và `open_decoder` chạy đồng bộ, bỏ qua cờ abort/closing; `Image::upload` chờ `PollType::Wait` không có timeout. Ảnh lớn hoặc GPU kẹt làm preview và lệnh đóng phải chờ hết. Ảnh hỏng không có negative cache nên mỗi lần render lại đọc và giải mã file từ đầu. Nên kiểm cờ trước khi giải mã, thêm timeout chờ, và nhớ lỗi theo dấu (cỡ + mtime) của file. Ghi ngày 2026-11-10.

### [Vừa] Project lưu đường dẫn gói người dùng cố định, đổi máy là mất gói và không biết bản cũ hay mới

`crates/gpufx-editor/src/package_import.rs:338` (`install_zip`) trả về `packages/gpufx/user/<id>.zip`, và project lưu đúng chuỗi đó. Mang project sang máy khác thì báo không tìm thấy gói mà không có cách lấy lại gói đó, và vì gói không có phiên bản nên không phân biệt được bản cũ với bản mới khi hai máy cài khác nhau. Nên cân nhắc lưu id kèm phiên bản trong project, hoặc nhúng gói vào project. Ghi ngày 2026-11-10.

### [Vừa] Tua clip GOP dài vượt ngưỡng đề xuất

Clip H.264 GOP 250 có B-frame tua p95 502 ms so với ngưỡng đề xuất 200 ms (`docs/plans/result-poc-phase0-vi.md`). Footage thật thường có GOP dài, nên giai đoạn sau cần proxy, cache hoặc giải mã trước. Ghi ngày 2026-11-09.

### [Vừa] Chờ decoder rảnh trước mỗi lần mix làm mất giải mã song song

`D3d12Decoder::wait_until_idle` ở `crates/poc/src/decoder.rs` và `SequentialDecoder::wait_idle` ở `crates/d3d12-backend/src/decoder.rs:389-401` chờ CPU trước mỗi `mix` để khung không còn bị decode khác tham chiếu. Phát p99 có lần lên 31,4 ms, sát ngưỡng 33 ms, chưa rõ nguyên nhân. Bản editor cần đồng bộ trên GPU hoặc tách luồng. Ghi ngày 2026-11-09.

### [Thấp] `still_dimensions` bỏ qua hướng EXIF

`crates/d3d12-backend/src/still.rs:54` (`still_dimensions`) đọc kích thước từ header mà không xoay theo EXIF, còn `read_still` xoay rồi; `crates/gpufx-editor/src/visuals/backend.rs:42` và tỉ lệ sticker trong thư viện dùng con số đó, nên ảnh chụp dọc có EXIF cho tỉ lệ ngược so với lúc render. Nên đọc orientation khi lấy kích thước. Ghi ngày 2026-11-10.

### [Thấp] Ảnh hỏng chỉ lộ khi render

`crates/d3d12-backend/src/paths.rs:277` chỉ kiểm được đường dẫn và phần mở rộng của ảnh, không kiểm nội dung; ảnh hỏng qua được validate và chỉ báo lỗi lúc tới khung có clip đó. Nên kiểm header lúc thêm clip hoặc lúc validate. Ghi ngày 2026-11-10.

### [Thấp] Đua khi khôi phục cài đặt dở

`crates/gpufx-editor/src/asset_install.rs:364` (`recover_interrupted_install`) khôi phục thư mục cài dở lúc khởi động mà có thể chạy cùng một lượt cài ở tiến trình editor khác. Nên giữ cùng lock cài đặt suốt lúc khôi phục. Ghi ngày 2026-11-10.

### [Thấp] Trần cache ảnh 1 GiB tính riêng từng compositor

`crates/d3d12-backend/src/compositor.rs` `STILL_CACHE_BYTES` áp cho từng `D3d12Compositor`, nên preview và export chạy cùng lúc có thể giữ gấp đôi. Nên dùng chung bộ đếm theo `Gpu` nếu số lượng compositor tăng. Ghi ngày 2026-11-10.

### [Thấp] Ảnh có tỉ lệ cực đoan bị overlay kẹp scale

`crates/d3d12-backend/src/compositor.rs` `place` truyền `height` theo `PlacedRect`, mà package overlay giới hạn `scale`/`height`; ảnh rất dài hoặc rất cao với placement lớn có thể bị kẹp nên méo so với khung đã chọn. Nên kiểm và báo hoặc tách nhiều bước. Ghi ngày 2026-11-10.

### [Thấp] Transition nhận màu premultiplied như straight

`crates/d3d12-backend/src/compositor.rs` `transition` (dòng gán `premultiplied = see_through`) đưa ảnh đã `place` vào gpufx với cờ `premultiplied` tắt, nên shader transition tính trên màu premultiplied (xem `external/FFmpegGpuFilter/crates/gpufx-core/src/frame.rs:258` nói effect phải thấy straight). Mix tuyến tính vẫn đúng, nhưng effect đổi màu làm sai vùng bán trong suốt. Nên báo `premultiplied` ở input transition. Ghi ngày 2026-11-10.

### [Thấp] MCP `clip_add` với asset:sticker không có placement mặc định

`crates/gpufx-mcp/src/tools.rs:163` (`clip_add`) qua `AgentEdit::ClipAdd` thêm sticker theo khớp cả khung, khác lúc thêm từ tab Sticker (nhỏ, căn giữa theo tỉ lệ thật). Agent phải tự gọi set placement. Nên dùng chung mặc định với `new_library_clip`. Ghi ngày 2026-11-10.

### [Thấp] Ảnh đổi trên đĩa vẫn dùng bản cũ cho tới khi rời clip

`crates/d3d12-backend/src/compositor.rs` `load_still` kiểm dấu file khi mở decoder, nên đang ở trong clip ảnh thì đổi file không được cập nhật tới khi clip được mở lại. Chấp nhận được vì rẻ; nên có tín hiệu làm mới nếu cần. Ghi ngày 2026-11-10.

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

`canvas` không được giải phóng trên đường lỗi (`crates/engine/src/render.rs:264-282`). `clip_graphs` không bao giờ bị loại bỏ (`crates/d3d12-backend/src/compositor.rs:186`), nên project nhiều clip giữ graph của mọi clip. Thư mục package mặc định là đường dẫn lúc build (`crates/gpufx-render/src/main.rs:68`), không chạy được khi file thực thi được chép đi. `Decoder::wait_idle` trong trait trung lập là khái niệm riêng của D3D12VA. Ghi ngày 2026-11-09.

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

### [Thấp] Hút mép khi kéo sang track mới hoặc thả từ thư viện không có đường báo hiệu

Kéo clip ra ngoài các hàng và kéo file từ thư viện vẫn hút mép lúc thả, nhưng không vẽ đường dọc nên clip có thể lệch tới 8 điểm so với con trỏ mà không thấy trước (`crates/gpufx-editor/src/ui/timeline_panel.rs`, `draw_drag_preview`, `handle_media_drop`). Nên làm để hành vi nhất quán với kéo clip trên hàng. Ghi ngày 2026-11-10.

### [Thấp] Hút đầu clip ở tốc độ khác 1 có thể hở dưới một bước lưới

Đầu clip bị engine làm tròn theo lưới tốc độ, nên khi hút vào mép clip trước vẫn có thể hở tới khoảng 11 ms trong khi đường báo hiệu vẽ như đã khít (`crates/gpufx-editor/src/timeline/snap.rs`, `lands`). Nếu mép đó trùng biên khung thì khung đó không có clip nào. Cần quyết định có chấp nhận không hay chỉ hút khi lưới đủ nhỏ. Ghi ngày 2026-11-10.

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

### [Thấp] Seek trong lúc clone project bị kẹp về điểm cuối cũ khi timeline dài ra

`crates/gpufx-editor/src/playback/thread.rs:241`: lệnh seek tới trong lúc `producer.set_project` đang clone bị kẹp theo điểm cuối của project cũ, nên timeline vừa dài ra thì seek quá điểm cuối cũ bị đưa về đó. Nên làm vì người dùng sẽ thấy tua sai vị trí đúng lúc vừa sửa. Ghi ngày 2026-11-09.

### [Thấp] Chế độ timer không có thiết bị âm thanh không bao giờ thử lại

`crates/gpufx-editor/src/playback/device.rs:37`: khi không mở được thiết bị thì clock chạy theo timer, và thiết bị không được thử lại cho tới lần mở lại `Playback`. Sửa kèm chú thích "stays until a device opens" ở `crates/gpufx-editor/src/editor.rs` cho đúng hành vi. Nên làm vì cắm tai nghe vào giữa chừng vẫn không có tiếng. Ghi ngày 2026-11-09.

### [Thấp] Bản render giữ lại khi đổi tên thất bại không được dọn

`crates/d3d12-backend/src/export.rs:128`: file đích bị khoá thì mỗi lần xuất lại để thêm một file `*.tmp.*` đầy đủ, không có gì dọn. Nếu `out` đổi tên xong mà file hash lỗi thì báo thất bại dù đích đã bị thay. Nên thử lại hoặc kiểm quyền ghi trước khi render. Ghi ngày 2026-11-09.

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


### [Thấp] Editor chết giữa lúc xuất để lại file tạm

`crates/gpufx-editor/src/export/child.rs:95`: job object giết `gpufx-render` ngay khi editor crash, nên file `*.tmp.mp4` cạnh file đích và project tạm `gpufx-export.*.tmp.json` trong thư mục temp còn lại. Nên dọn ở lần mở editor sau, chỉ xoá file có PID không còn sống. Đóng app lúc đang xuất cũng chỉ chờ 2 s (`crates/gpufx-editor/src/export.rs:29,141`), nên một lần huỷ chậm trên renderer thật có thể để lại đúng các file đó; chưa đo. Ghi ngày 2026-11-09.


### [Thấp] Chưa kiểm chứng việc cô lập khi GPU reset bằng TDR cưỡng bức

`crates/gpufx-editor/src/export.rs:1`: việc tách export sang tiến trình con mới chứng minh được là crash trong tiến trình con không kéo editor chết. Một lần reset adapter có thể ảnh hưởng nhiều tiến trình cùng lúc, và `crates/gpufx-editor/src/window.rs:711-714` thoát app khi redraw lỗi (gọi ở `window.rs:758`), nên một TDR trong lúc xuất vẫn có thể đóng editor. Nên thử bằng TDR cưỡng bức trên máy thật rồi quyết định có giữ app sống khi mất device hay không. Ghi ngày 2026-11-09.

### [Thấp] Server MCP bị kill giữa lúc xuất để lại project tạm

`crates/gpufx-editor/src/export/child.rs:72` (`TemporaryProject`): tiến trình con tự dừng sạch khi stdin đóng và không để lại file video tạm, nhưng project tạm `gpufx-export.*.tmp.json` trong thư mục temp thì không ai xoá khi `gpufx-mcp` chết. Tiến trình con kẹt quá hạn 30 s (`crates/gpufx-render/src/main.rs:24,119`) tự thoát bằng `process::exit`, nên còn để lại cả file `*.tmp.mp4` cạnh file đích. Nên dọn chung với việc dọn file tạm của editor. Ghi ngày 2026-11-09.

### [Thấp] `export_start` chỉ kiểm `overwrite: false` lúc bắt đầu

`crates/gpufx-editor/src/agent/run.rs:358`: file đích xuất hiện sau bước kiểm (app khác tạo ra trong lúc render) vẫn bị bước đổi tên cuối cùng ghi đè dù `overwrite` là false. Nên để bước đổi tên từ chối ghi đè khi không được phép. Ghi ngày 2026-11-09.

### [Thấp] Tắt quyền agent trong lúc chụp chặn UI 1 giây và bật lại báo sai lỗi

`crates/gpufx-editor/src/agent/gateway.rs:312` (`Drop`): luồng pipe đang chờ một lần chụp thì UI đứng tới 1 s, và luồng đó còn giữ pipe nên bật lại ngay sẽ báo lỗi tạo pipe (kiểu "có editor khác đang mở") thay vì nói là chờ một chút. Nên tách việc đóng pipe khỏi việc chờ, hoặc thử lại khi bật. Ghi ngày 2026-11-09.

### [Thấp] Pipe của agent dùng PIPE_NOWAIT và thăm dò 5 ms

`crates/gpufx-editor/src/agent/gateway.rs:126`: `PIPE_NOWAIT` là chế độ Microsoft chỉ giữ để tương thích, và vòng thăm dò đánh thức luồng liên tục kể cả khi không có client. Nên đổi sang overlapped I/O, cũng giúp huỷ được lần ghi/đọc đang chờ. Ghi ngày 2026-11-09.

### [Thấp] Client của pipe đọc không có timeout

`crates/gpufx-mcp/src/session.rs:208`: `LiveSession` chờ câu trả lời vô hạn, nên cửa sổ treo thì tool của agent cũng treo theo. Nên có hạn chờ và trả `no_app`, nhưng phải trừ đủ thời gian cho một lần chụp dài. Ghi ngày 2026-11-09.

### [Thấp] Hai chương trình tranh nhau file khoá thì cửa sổ vẫn cho sửa

`crates/gpufx-editor/src/window.rs:585` (`sync_project_paths`): nếu không lấy được khoá sau khi project đã đổi (ca hiếm, vì lúc mở đã kiểm trước) thì chỉ hiện thông báo, cửa sổ vẫn sửa và lưu được. Open từ giao diện cũng đi qua nhánh này, nên project đã được thay rồi mới biết khoá thất bại. Nên chuyển sang chỉ đọc hoặc bỏ project nếu ca này gặp thật. Ghi ngày 2026-11-09.

### [Vừa] MCP chưa được thử với client MCP thật và cửa sổ thật

`crates/gpufx-editor/src/window.rs:241` (`serve_agent`): test đã đi qua pipe thật, `EditorState` và luồng preview thật, nhưng không có cửa sổ winit. Bật tuỳ chọn trong menu, dấu báo agent, lệnh tới khi cửa sổ thu nhỏ và preview cập nhật sau lệnh agent mới chỉ thử được bằng tay. Tiêu chí "Dùng thật" của đặc tả vẫn còn mở. Ghi ngày 2026-11-09.

### [Vừa] Lưu hoặc xuất có thể ghi đè file bên trong package dạng thư mục

`crates/d3d12-backend/src/export.rs:117` (`overwritten_input`): chỉ so đường dẫn đích với chính thư mục package, nên `project_save` với `overwrite: true` vào `<package>/manifest.json` làm hỏng package đang dùng, `export_start` hở tương tự. Nên từ chối mọi đích nằm dưới một package thư mục của project. Ghi ngày 2026-11-09.

### [Thấp] So file cùng một đích qua admin share không nhận ra

`crates/d3d12-backend/src/export.rs:87` (`same_file`): `\\localhost\c$\m\a.mp4` không trùng `C:\m\a.mp4` sau `canonicalize`, nên kiểm tra ghi đè file nguồn bị vượt. Cần quyền vào admin share nên khó gặp; chặn triệt để thì so volume serial và file ID. Ghi ngày 2026-11-09.

### [Thấp] Capture xếp hàng trước khi luồng preview chết vẫn treo

`crates/gpufx-editor/src/preview/worker.rs:167`: task vào queue trong lúc `make()` đang chạy rồi `make()` panic thì không được trả lời, `PendingFrame::finish` (`crates/gpufx-editor/src/agent/run.rs:57`) chặn luồng pipe tới khi worker bị drop. Nên xả queue khi luồng thoát, ví dụ bằng guard trong closure spawn. Ghi ngày 2026-11-09.

### [Thấp] Hạn 30 giây khi ghi pipe tính trên cả câu trả lời

`crates/gpufx-editor/src/agent/gateway.rs:231`: client vẫn đọc đều mà ảnh rất lớn (PNG base64 khoảng trên 120 MB, ước tính từ đọc code) thì bị cắt và client báo nhầm `no_app`. Nên tính hạn từ lần ghi có tiến triển cuối. Ghi ngày 2026-11-09.

### [Thấp] Editor chạy quyền admin có thể bị MCP thường từ chối

`crates/gpufx-editor/src/agent/gateway.rs:49`, phía client ở `crates/gpufx-mcp/src/session.rs`: kiểm chủ server cần mở token của tiến trình editor, từ medium integrity lên high integrity nhiều khả năng bị từ chối. Đây là fail-closed có thông báo, chưa chạy thử; ghi lại để biết khi gặp. Ghi ngày 2026-11-09.

### [Thấp] Comment đặt sai chỗ ở test preview

`crates/gpufx-editor/tests/preview_gpu.rs:292`: comment về `try_lock_submits` nằm trên test capture mới thay vì trên test nó mô tả ở dòng 311. Ghi ngày 2026-11-09.

### [Vừa] Preview ở tốc độ cao có thể không kịp giải mã

Ở 10x mỗi khung xuất cần `crates/d3d12-backend/src/decoder.rs:525` giải mã xuyên qua khoảng 10 khung nguồn, trừ khi nhảy quá 2 giây tới keyframe. Nguồn 4K dễ không kịp thời gian thực và preview sẽ rớt khung (xuất file vẫn đúng). Cần đo trên máy thật và cân nhắc giải mã chỉ khung intra khi tua nhanh. Khung không kịp đã được bỏ qua không tốn GPU (`LatestSlot`, `crates/gpufx-editor/src/preview/worker.rs:118`), nhưng giải mã chậm hơn thời gian thực trên clip GOP dài thì độ trễ của hình tăng dần tới khi `forward_seek_pays` cho tua; trong lúc phát, `ABORT_GRACE` 150 ms (`crates/gpufx-editor/src/preview/backend.rs:19`) còn huỷ lượt giải mã nếu vừa có khung trong 500 ms (`PUBLISH_FRESH_WITHIN`, `crates/d3d12-backend/src/decoder.rs:42`), nên ca này chỉ cập nhật hình khoảng mỗi nửa giây (suy từ đọc code, chưa đo). Ghi ngày 2026-11-10.

### [Thấp] Đoạn ghép không căn pha mỗi 1,4 giây trong bộ giãn tiếng

`crates/engine/src/stretch.rs:39` đặt mỗi 64 grain một grain không căn với grain trước, để tua chỉ phải dựng lại tối đa 1,4 giây. Đo sine với RMS cửa sổ 256 mẫu: trước khi xử lý, hụt tới 18 dB tại đó (110 Hz ở 4x, phổ biến 2 đến 10 dB); `apply_join_gain` bù lại nhưng trần gain chỉ là 2, vì một gain chung cho mọi tần số sẽ làm thành phần không bị triệt tiêu to vọt lên (`a_key_join_on_cancelling_bass_does_not_burst_what_did_not_cancel`). Với trần này còn tối đa 4,7 dB ở 0,5x, 0,1x, 2x, test `a_key_grain_does_not_dip_the_level` giữ ngưỡng 6 dB. Ở 110 Hz 4x, trước khi hạ trần đo được 8,2 dB, theo review là do tín hiệu đổi pha gần 166 độ trong khoảng 80 mẫu chứ không phải thiếu năng lượng. Key grain nằm trong 21 ms cuối file có thể bị nâng thêm tới 3 dB vì bù biên cộng với gain ghép (`crates/engine/src/stretch.rs:173`, suy từ đọc code). Còn lại là một chỗ nhảy pha, nghe như tiếng nhấp nhẹ ở âm liên tục. Chưa nghe thử trên nhạc thật; nếu lộ rõ thì tăng khoảng hoặc giữ vị trí grain xuyên suốt clip. Ghi ngày 2026-11-10.

### [Thấp] Cắt đuôi và transition không giới hạn theo độ dài nguồn

`crates/gpufx-editor/src/timeline/drag.rs:81-87`: kéo cuối clip và thêm transition chỉ bị chặn bởi clip kế bên, không bị chặn bởi độ dài nguồn, vì project không lưu độ dài đó. Ở tốc độ cao ảnh hưởng nhân lên (10x đọc quá nguồn gấp 10 lần), nên phần thiếu phát ra im lặng hoặc ảnh đứng mà không có báo gì. Cần lưu độ dài nguồn vào project hoặc tra từ thư viện media khi sửa. Ghi ngày 2026-11-10.

### [Thấp] Grain cuối của clip tua có thể đọc quá điểm ra

`crates/engine/src/audio_schedule.rs:18`: grain cuối đọc quanh vị trí danh nghĩa nên có thể lấy tiếng quá `source_end` của clip tới khoảng 192 ms ở 10x. Bản sửa mức tiếng đầu/cuối chỉ biết ranh giới của file, không biết điểm ra của clip, và chặn theo clip còn phải tính cả cửa sổ transition. Đáng làm khi có âm khác ngay sau điểm ra. Ghi ngày 2026-11-10.

### [Thấp] Tách hoặc cắt clip đang tua có một mẫu lệch vị trí nguồn tiếng

Ở tốc độ khác 1, sau khi tách, vị trí âm danh nghĩa của nửa sau khác tối đa 1 mẫu (dưới 21 µs) so với trước khi tách, và chuỗi grain bắt đầu lại ở chỗ cắt (`crates/engine/src/edit.rs` `cut_keeps_samples`). Vị trí khung hình vẫn chính xác. Không ảnh hưởng thời lượng hay đồng bộ. Ghi ngày 2026-11-10.

### [Thấp] Nút reset trong panel Clip vẫn sáng khi không đi được đâu

`crates/gpufx-editor/src/ui/inspector.rs:104`: nút reset của Start of video và Duration of video vẫn bấm được khi clip kế tiếp đã sát cạnh hoặc clip có transition ở cạnh đó, bấm thì không đổi gì hoặc bị từ chối. Không hại dữ liệu; nên tính trạng thái nút từ giới hạn thật (`clamp_source_in`, `clamp_trim`). Clip trên track audio lấy tiếng từ file video cũng chưa có reset độ dài, vì thư viện chỉ giữ độ dài hình. Ghi ngày 2026-11-10.

### [Thấp] Thả file có cả hình và tiếng lên track audio xử lý khác nhau theo nguồn

`crates/gpufx-editor/src/editor.rs` `drop_media`: thả từ thư viện bị từ chối vì mục thư viện mang loại Video, còn thả từ Explorer thì nhận và lấy tiếng (`add_dropped_file`). Nên thống nhất, vì cùng một file mà kết quả khác nhau. Ghi ngày 2026-11-10.

### [Thấp] Giới hạn cứng của overlay làm méo hình trong im lặng khi hình dạng cực đoan

`crates/d3d12-backend/src/compositor.rs:468-470` (`place`): `set_param` của overlay kẹp giá trị theo manifest (`height` trên 1000, `scale` dưới 0,01), không báo lỗi, nên một placement cực đoan cho ra hình bị méo hoặc sai kích thước mà không ai biết. Chỉ gặp khi clip rất dẹt hoặc rất cao đặt ở khung rất khác hình dạng. Nên kiểm trước khi gọi `set_param` và trả lỗi hoặc ghi cảnh báo. Ghi ngày 2026-11-10.

### [Thấp] Khung đỏ quá nhỏ thì các tay nắm che mất thân khung

`crates/gpufx-editor/src/ui/placement.rs:11,177-181`: vùng bắt chuột của tay nắm là 14 px (`GRIP_HIT_SIZE`), nên khi khung nhỏ hơn khoảng 28 px thì các vùng này phủ kín thân và không còn chỗ để kéo di chuyển cho tới khi phóng to khung lên. Nên thu vùng bắt của tay nắm theo cỡ khung, hoặc nhận kéo thân bằng phím hay bằng ô trong inspector. Ghi ngày 2026-11-10.

### [Thấp] Hình nguồn bị lấy mẫu hai lần khi phóng to

`crates/d3d12-backend/src/compositor.rs:525-531` (`build_clip_graph`) rồi `place` (dòng 461): nguồn được scale về cỡ output trước, sau đó mới overlay lên vị trí. Phóng to nguồn 4K trong project 1080p thì hình bị mềm vì dữ liệu 4K đã mất ở lần scale đầu. Nên scale thẳng về cỡ đích của placement trong một lần khi clip có placement. Ghi ngày 2026-11-10.

### [Thấp] Chưa có tool MCP đặt placement của clip

`crates/gpufx-mcp/src/`: agent không đặt hay xoá được `Clip.placement` (vị trí và cỡ clip trong khung); chỉ người dùng làm được bằng khung đỏ trên player và nút Default ở panel Clip. Nên thêm tool theo mẫu các tool sửa clip khác, dùng `set_placement` của engine. Ghi ngày 2026-11-10.

### [Thấp] Kẹp file trên 60 fps về 60 fps mà người dùng không biết

`crates/gpufx-editor/src/project_file.rs:120`: mở file có `frame_duration` dưới 1500 tick chỉ ghi stderr, project không bị đánh dấu chưa lưu, nên có thể xuất 60 fps mà không hay. Nên báo ở thanh trạng thái hoặc đánh dấu chưa lưu. CLI `gpufx-render` thì từ chối chính file đó, chờ user chốt có dùng chung cách kẹp không. Ghi ngày 2026-11-10.

### [Thấp] Test preview bỏ khung chưa chứng minh được `LatestSlot`

`crates/gpufx-editor/tests/preview_drop.rs:109`: đồng hồ giả chỉ tăng trong `produce`, nên mỗi lần render chỉ có một yêu cầu mới, thay slot bằng hàng đợi FIFO thì test vẫn xanh. Nên gửi nhiều yêu cầu trong lúc `produce` đang chặn rồi khẳng định khung giữa không được render. Ghi ngày 2026-11-10.

### [Thấp] Preview effect co ảnh đầu ra chưa tốt khi package đổi kích thước

`crates/d3d12-backend/src/effect_preview.rs`, hàm `resize_rgba`: nội suy chỉ lấy 2x2 điểm nên thu nhỏ mạnh bị răng cưa, output không vuông bị kéo méo, và nội suy trên alpha thẳng làm viền lem màu. Hiện chỉ package kiểu `scale` đi vào đường này. Nên dùng box filter, premultiply trước khi lọc và letterbox. Ghi ngày 2026-11-10.

### [Thấp] `luma-matte` hiện trong danh sách effect nhưng không có preview

`crates/gpufx-editor/src/packages.rs:21-26` xếp package 1 input vào effect mà không xét số output, còn `crates/d3d12-backend/src/effect_preview.rs` từ chối package có 2 output nên `ffmpeg/luma-matte` sẽ ra ô lỗi. Nên thống nhất: lọc ở `packages.rs`, hoặc render output 0. Chưa kiểm editor có render được package này không. Ghi ngày 2026-11-10.

### [Thấp] Still và loop của preview dùng chung bản ghi lỗi

`crates/gpufx-editor/src/effect_previews/key.rs` `preview_key` không có loại ảnh nên `.attempt` và `.failed` dùng chung cho still và loop: một lần lỗi mỗi loại tính thành hai lần, và loop hỏng chặn cả still. Nên thêm loại vào tên file hoặc nội dung bản ghi. Ghi ngày 2026-11-10.

### [Thấp] File tạm `.importing-<pid>` mồ côi khi app bị kill giữa lúc ghi

`crates/gpufx-editor/src/package_import.rs:283` (`write_replacing`) ghi qua file `<id>.zip.importing-<pid>` rồi đổi tên. Nếu app bị kill giữa chừng thì file tạm nằm lại trong `packages/gpufx/user` và không có chỗ nào dọn. Nên quét và xoá file `.importing-*` cũ khi khởi động. Ghi ngày 2026-11-10.

### [Thấp] Zip có thể đổi trong lúc hộp thoại ghi đè đang mở

`crates/gpufx-editor/src/tasks.rs:330` (`PromptAnswered`) và `install_zip` (`package_import.rs:304`): câu hỏi ghi đè dựa trên lần đọc đầu, còn lần cài thứ hai đọc lại file. Nếu file zip bị thay giữa hai lần thì lần cài thứ hai có thể thay một id khác mà không hỏi. Nên giữ byte đã kiểm hoặc so lại id trước khi ghi đè. Ghi ngày 2026-11-10.

### [Thấp] Preview âm thanh không có waker, bấm Nghe liên tiếp xếp hàng thay vì gộp

`crates/gpufx-editor/src/playback/preview.rs:98` (`run`, nhánh `Command::Play`) và `:73` (`play`): luồng preview không đánh thức cửa sổ, nên icon và lỗi chỉ hiện ở lần nhập kế tiếp; bấm Nghe nhiều lần liên tiếp xếp từng lệnh vào hàng đợi, mỗi lệnh mở lại thiết bị, thay vì chỉ giữ lệnh cuối. Nên cho luồng gọi `request_redraw` khi đổi trạng thái và gộp các `Play` đang chờ về lệnh cuối. Ghi ngày 2026-11-10.

### [Thấp] Gói hỏng ở root đầu bị thư viện bỏ qua nhưng vẫn thắng khi phân giải

`crates/gpufx-editor/src/audio_library.rs:123` (`scan_audio_packs`, bỏ gói hỏng, không thêm vào `seen`) và `crates/d3d12-backend/src/paths.rs:213` (`asset_path`, chỉ bỏ qua root khi thiếu `manifest.json`): gói có manifest hỏng ở root đầu không được liệt kê, nhưng với manifest còn đó thì clip vẫn phân giải vào nó. Khi gói âm thanh đi kèm app, một gói người dùng nhập cùng id sẽ bị che mà không báo. Nên để hai bên cùng một quy tắc: gói hỏng cũng che, kèm báo lỗi, hoặc cùng bỏ qua. Ghi ngày 2026-11-10.

### [Thấp] `probed_duration` còn trong cache sau khi gói bị ghi đè, và mỗi lần Add thêm cả âm thanh vào danh sách phiên

`crates/gpufx-editor/src/media.rs:57` (`probed_duration`) tìm theo nguồn trong danh sách phiên và không biết gói đã bị ghi đè nên giữ độ dài cũ; `crates/gpufx-editor/src/editor.rs:1289` (`place_library_audio`) và `:1308` (`add_probed_library_item`) đưa mục thư viện vào danh sách phiên mỗi lần Add, nên danh sách phiên phình lên theo số lần dùng. Nên bỏ mục cache của gói khi `AudioPackInstalled`, và không thêm âm thanh thư viện vào danh sách phiên. Ghi ngày 2026-11-10.

### [Thấp] Gói đọc clip_time/clip_duration mà có `gpufx_output_size` báo lỗi render từ khung thứ hai

`crates/d3d12-backend/src/compositor.rs:671` (`set_clip_clock`) chạy sau `needs_rebuild` (`:372`), còn `external/FFmpegGpuFilter/crates/gpufx-core/src/runtime.rs:869` tính lại kích thước ra từ tham số: ở khung đầu đồng hồ clip chưa được đặt, từ khung thứ hai `needs_rebuild` thấy kích thước đổi theo đồng hồ và graph dựng lại với đồng hồ cũ nên báo lỗi. Nên đặt đồng hồ clip trước `needs_rebuild`, hoặc cho `needs_rebuild` bỏ qua tham số đồng hồ. Ghi ngày 2026-11-10.

### [Thấp] `clock_params` chỉ so tên tham số, không kiểm kiểu

`crates/d3d12-backend/src/compositor.rs:319` (`set_clip_clock`, dùng kết quả của `clock_params` ở `:291`): gói khai báo `clip_time` hoặc `clip_duration` kiểu expr hoặc enum sẽ lỗi ở mọi khung, `max` khai báo hẹp bị kẹp âm thầm làm đồng hồ dừng sớm, kiểu int mất phần lẻ. Nên kiểm kiểu float khi đọc manifest và báo lỗi rõ khi sai, thay vì lỗi lặp lại mỗi khung hoặc sai lặng lẽ. Ghi ngày 2026-11-10.

### [Thấp] Transition khai báo clip_time/clip_duration: tên bị ẩn nhưng không có đồng hồ

`crates/gpufx-editor/src/params.rs:44`, `crates/gpufx-editor/src/agent/run.rs:125` và `:260` ẩn tên `clip_time`/`clip_duration` khỏi inspector và MCP bất kể vai trò của gói, nhưng `crates/d3d12-backend/src/compositor.rs:602` chỉ cấp đồng hồ cho effect của clip, không cấp cho transition, và `TransitionAdd` không lọc nên giá trị người dùng nhập vẫn được lưu và áp. Kết quả: tham số không thấy được nhưng vẫn chi phối hình. Nên lọc theo vai trò (chỉ ẩn ở effect clip), hoặc cấp đồng hồ cho transition, và để `TransitionAdd` bỏ các tên này. Ghi ngày 2026-11-10.

### [Vừa] Transition ghi đè alpha giữa hai sticker khác hình dạng cho bóng đen, và làm mất độ mờ hoặc ra góc đen khi clip có opacity hoặc xoay

`crates/d3d12-backend/src/alpha_merge.rs:48` lấy alpha kết quả là `max(a_from, a_to)`. Tại pixel một bên đục, bên kia trong suốt, màu premultiplied mờ dần nhưng alpha giữ 1, nên silhouette tối dần về đen rồi biến mất khi transition hết. Chưa thấy transition có sẵn nào ghi alpha 1, chỉ gói tự viết mới dính. Phần mở rộng: clip có opacity dưới 1 hoặc xoay (góc ảnh sau xoay trong suốt) đi qua transition ghi alpha 1 (`crates/d3d12-backend/src/compositor.rs:964`, `transition`) cũng bị mất trong suốt: độ mờ biến mất hoặc các góc sau xoay ra đen. Nên làm: chạy transition lần hai trên matte (alpha của mỗi bên đưa vào rgb) và lấy kết quả làm alpha. Không thay `max` bằng mix theo progress: sai với wipe, pixel đang hiện hẳn một bên sẽ bị nửa alpha. Ghi ngày 2026-11-10.

### [Thấp] Probe alpha là suy đoán một lần cho mỗi graph

`crates/d3d12-backend/src/compositor.rs:800` (`writes_alpha`) probe trên ảnh đen trong suốt, với clock của frame dựng đầu tiên, và kết quả được cache theo khoá không chứa số. Gói bên thứ ba ghi alpha theo tham số hoặc theo thời gian, hay chỉ phủ đục một phần, sẽ bị đoán sai và vùng trong suốt thành khối đặc. Effect vừa di chuyển hình vừa ghi alpha 1 bị cắt theo hình gốc. Probe dùng giá trị keyframe của frame dựng đầu tiên (độ mờ, xoay, tham số effect), không phải giá trị mặc định, nên một clip có keyframe cho ra kết quả probe tuỳ vào khung nào được dựng trước. Nên probe ở giá trị mặc định. Mỗi `ClipKey` có alpha tốn một lần probe chặn thread. Nên làm nếu gói như vậy xuất hiện hoặc timeline có hàng chục sticker: cho manifest khai báo việc giữ alpha. Ghi ngày 2026-11-10.

### [Vừa] Không đặt được keyframe trong nửa cửa sổ transition nằm ngoài clip

`crates/engine/src/edit.rs:739` (`inside_clip`) chỉ cho đặt keyframe trong [0, duration), còn `crates/engine/src/schedule.rs:165` (`sample_at`) cho clip chạy tiếp ngoài đó trong cửa sổ transition: clip giữ nguyên giá trị của keyframe đầu hoặc cuối. Người dùng không keyframe được phần cửa sổ ấy, nên một hiệu ứng như làm mờ dần xuyên qua transition không dựng được bằng keyframe. Nên làm nếu có người cần: cho phép `at` ngoài clip khi nằm trong cửa sổ transition của clip, và sửa `keyframe_times` cùng thanh keyframe trên timeline cho khớp. Ghi ngày 2026-11-10.

### [Thấp] Keyframe tham số float mà hàm kích thước đọc làm dựng lại graph mỗi khung

`crates/d3d12-backend/src/compositor.rs:940` (`needs_rebuild`): tham số float mà `gpufx_output_size` hay hàm kích thước của gói đọc, khi có keyframe thì kích thước ra đổi từng khung và graph bị dựng lại mỗi khung. Nên làm nếu có gói như vậy: tách tham số ảnh hưởng kích thước khỏi tham số được keyframe, hoặc từ chối keyframe cho chúng. Ghi ngày 2026-11-10.

### [Thấp] Kiểm tra project không xét tên và kiểu tham số của keyframe trong file

`crates/engine/src/validate.rs:342` chỉ kiểm thời gian, thứ tự, hữu hạn của keyframe effect; file tay sửa có thể mang keyframe cho tham số không tồn tại hoặc không phải float, và nó bị bỏ qua lặng lẽ khi render. Chỉ lối MCP (`check_keyframe_target`) kiểm tên và kiểu. Nên làm: kiểm theo catalog khi mở file và báo như tham số thường. Ghi ngày 2026-11-10.

### [Thấp] Chưa keyframe được tham số Int, Vec, Color và Enum

`crates/engine/src/keyframe.rs` (`KeyframeTarget::EffectParam`, giá trị `f64`) và `crates/gpufx-editor/src/agent/run.rs` (`check_keyframe_target`) chỉ cho tham số float. Màu và vector là thứ hay cần chuyển động nhất. Nên làm: khoá giá trị thành mảng số và nội suy theo thành phần, màu nội suy trong không gian tuyến tính. Ghi ngày 2026-11-10.

### [Thấp] Thu nhỏ không có mipmap nên ảnh xoay nhấp nháy

`crates/d3d12-backend/src/rotated_place.rs:57` lấy mẫu bilinear từ ảnh gốc. Khi thu nhỏ mạnh, mỗi pixel đích chỉ đọc 4 texel và bỏ qua phần còn lại, nên cạnh và chi tiết mảnh nhấp nháy lúc xoay. Nên làm: dựng mipmap hoặc lấy mẫu nhiều điểm theo hệ số thu nhỏ. Ghi ngày 2026-11-10.

### [Thấp] Vùng kéo khung xoay là hộp bao ngoài

`crates/gpufx-editor/src/ui/placement.rs:188` đặt vùng nhận kéo thân khung theo hộp bao (`bounds(corners)`) của khung đã xoay, nên bấm vào góc trống của hộp bao vẫn kéo clip. Nên làm: kiểm điểm trong tứ giác xoay thay vì hộp bao. Ghi ngày 2026-11-10.

### [Thấp] Timeline sắp xếp và cấp phát danh sách keyframe mỗi khung cho mỗi clip

`crates/gpufx-editor/src/ui/timeline_panel.rs:404` gọi `Clip::keyframe_times` (`crates/engine/src/keyframe.rs`), vốn gom, sắp xếp và bỏ trùng một `Vec` mới ở mỗi lần vẽ của mỗi clip. Vài clip thì không đáng kể, hàng trăm clip có keyframe thì tốn. Nên làm nếu timeline dài gây giật: cache theo revision của project hoặc chỉ tính cho clip đang trong khung nhìn. Ghi ngày 2026-11-10.

### [Thấp] Thông báo không đổi được tốc độ vẫn gọi clip chữ là "ảnh"

`crates/gpufx-editor/src/editor.rs:171` (`refusal_message`) đổi `EditError::SpeedOfImage` thành `Message::EditSpeedOfImage`, mà `crates/gpufx-editor/src/i18n/vi.rs:202` và `en.rs:219` ghi "ảnh không có tốc độ phát". Engine trả lỗi này cho mọi clip tĩnh, kể cả clip chữ, nên người dùng đổi tốc độ clip chữ sẽ đọc thấy chữ "ảnh". Nên tách thông báo theo loại clip (hoặc đổi thành "clip tĩnh"). Ghi ngày 2026-11-11.

### [Thấp] Kéo mép đầu clip tĩnh không lùi được trước điểm bắt đầu

`crates/gpufx-editor/src/timeline/drag.rs:75` (`clamp_trim`) lấy `source_limit` từ `source_in` cho mọi clip, nên mép đầu của clip ảnh/chữ không kéo lùi được trước `start` dù engine (`trim_start`) cho phép clip tĩnh dài ra phía trước. Có từ trước bước chữ. Nên bỏ giới hạn nguồn khi `clip.is_still()`. Ghi ngày 2026-11-11.

### [Thấp] gpufx-text chưa tự ngắt dòng, emoji vẽ bằng màu chữ, đường font hệ thống chưa có test

`crates/gpufx-text/src/lib.rs` (`layout`, `buffer.set_wrap(Wrap::None)`) chỉ xuống dòng ở ký tự xuống dòng, chưa ngắt theo độ rộng; glyph màu (emoji, `SwashContent::Color`) và `SwashContent::SubpixelMask` ở `draw` chỉ lấy kênh cuối làm độ phủ nên emoji ra một màu của chữ, còn mặt nạ subpixel (ba kênh độ phủ, không có alpha) bị đọc sai kênh; test ở `crates/gpufx-text/tests/rasterize.rs` chỉ dùng `TextRasterizer::embedded_only()`, đường `TextRasterizer::new()` quét font thật chưa được kiểm. Nên thêm ngắt dòng theo độ rộng hộp, vẽ glyph màu đúng màu, và một test (có thể tự bỏ qua) cho font hệ thống. Ghi ngày 2026-11-11.

### [Thấp] Locale của gpufx-text lấy từ `LANG` nên trên Windows gần như luôn là en-US

`crates/gpufx-text/src/lib.rs` (`sys_locale`) chỉ đọc biến `LANG`, mà Windows thường không đặt, nên `FontSystem` luôn nhận `en-US`. cosmic-text dùng locale để chọn font fallback, nên chữ Hán có thể rơi vào font Nhật hoặc Hàn (tự hình khác) thay vì font Trung trên máy người dùng Trung, và ngược lại. Nên đọc locale hệ thống (`GetUserDefaultLocaleName` hoặc crate `sys-locale`), hoặc cho clip chữ khai báo ngôn ngữ. Ghi ngày 2026-11-11.

### [Thấp] Ảnh chữ đi vòng premultiplied → straight u8 → premultiply nên mất chính xác ở alpha thấp

`crates/d3d12-backend/src/still.rs` (`premultiplied_still`) chia màu cho alpha ở u8 để thành straight, rồi GPU nhân lại; ở alpha thấp (mép chữ, bóng mờ) phép chia làm tròn thô nên mép có thể lệch màu hoặc sọc, cộng thêm việc gpufx-text trộn trong không gian sRGB chứ không tuyến tính. Nên upload thẳng ảnh premultiplied (đánh dấu still là premultiplied) và cân nhắc trộn tuyến tính. Ghi ngày 2026-11-11.

### [Thấp] Một `Mutex` toàn cục quanh `FontSystem` làm export và preview vẽ chữ tuần tự

`crates/gpufx-text/src/lib.rs` (`shared`) trả một `Arc<Mutex<TextRasterizer>>` cho mọi compositor, nên preview và export cùng lúc phải chờ nhau mỗi lần đo hoặc vẽ chữ (vẽ chữ lớn tốn hàng trăm ms). Nên tách phần vẽ (mặt nạ, viền, bóng) ra ngoài khoá, hoặc cho mỗi compositor một `FontSystem` dùng chung `fontdb::Database`. Ghi ngày 2026-11-11.

### [Thấp] Cache glyph của `SwashCache` không bao giờ được dọn

`crates/gpufx-text/src/lib.rs` (`draw`, `self.cache.get_image`) giữ ảnh glyph theo cỡ chữ mãi mãi; đổi cỡ chữ hay scale (mỗi bậc 2^(1/4)) thêm một bộ glyph, chữ lớn mỗi glyph tới vài MB. Nên dọn `image_cache` khi vượt một ngưỡng byte, hoặc dùng `get_image_uncached` cho cỡ lớn. Ghi ngày 2026-11-11.

### [Thấp] Clip chữ chưa có vị trí mặc định theo cỡ chữ

Clip chữ không có `placement` thì được dàn vừa khung như ảnh (`PlacedRect::of` ở `crates/engine/src/project.rs`), nên `size` không quyết định cỡ hiện ra; compositor còn phải rasterize ở scale trần 4 (`MAX_TEXT_SCALE`). Bước UI thêm chữ nên đặt `placement` theo cỡ tự nhiên (rộng = rộng ảnh ở scale 1 / rộng khung, căn giữa), lấy từ `crates/gpufx-editor/src/text_shapes.rs`. Ghi ngày 2026-11-11.

### [Thấp] Agent tool chưa có lệnh thêm clip chữ

`crates/gpufx-editor/src/agent/run.rs:201` (dựng clip cho lệnh thêm clip) chỉ biết clip từ file (video, âm thanh, ảnh); agent không có lệnh nào tạo clip chữ (`source` `text:` kèm `TextContent`) hay sửa nội dung chữ, nên không tự động hoá được phụ đề hay tiêu đề qua agent. Nên thêm lệnh thêm và sửa clip chữ (gọi `engine::add_text_clip` / `set_text`) cùng mã lỗi tương ứng trong `agent/protocol.rs`. Ghi ngày 2026-11-11.
