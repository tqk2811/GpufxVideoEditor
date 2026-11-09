# Đặc tả giai đoạn 2: audio

Ngày lập: 2026-11-09. Trạng thái: **đề xuất, chưa code**. Kế hoạch tổng ở [plan-video-editor-vi.md](plan-video-editor-vi.md), engine ở [spec-engine-phase1-vi.md](spec-engine-phase1-vi.md).

Mục tiêu: file xuất có tiếng đã cắt, trộn và fade đúng theo timeline, và engine có đồng hồ audio để giai đoạn UI dùng làm đồng hồ chính cho preview.

## Quyết định cần chốt

Các mục dưới đây là đề xuất, chỉ user chốt được.

| Vấn đề | Đề xuất | Lý do |
|---|---|---|
| Nơi đặt tiếng | Clip video mang tiếng của chính nó, thêm track chỉ có audio | Giống cách dùng kiểu CapCut, nhạc nền không cần khung hình |
| Định dạng trộn | 48 kHz, stereo, số thực 32-bit | 1 mẫu đúng bằng 1,875 tick, đổi qua lại bằng số nguyên |
| Đầu ra | AAC trong MP4, encoder có sẵn của FFmpeg | Có trong cả bản LGPL, không chờ bản GPL |
| Tiếng trong transition | Crossfade đẳng công suất theo đúng cửa sổ transition của video | Tổng âm lượng cảm nhận ổn định khi hai nguồn khác nhau |
| Vượt ngưỡng khi trộn | Kẹp về [-1, 1] và đếm số mẫu bị kẹp trong báo cáo | Đơn giản, lặp lại được; limiter làm sau |
| Thiết bị phát thật | Giai đoạn 2 chỉ có đồng hồ và API kéo mẫu, nối loa ở giai đoạn UI | Chưa có preview nào để đồng bộ; đồng hồ test được bằng thiết bị giả |

## Phạm vi

Có:

- Clip có `gain_db`, `muted`, `fade_in`, `fade_out`. Track có `kind` là `video` (mặc định) hoặc `audio`.
- Trộn nhiều nguồn, resample về định dạng trộn, crossfade trong transition.
- Xuất video kèm AAC. CLI `gpufx-render` xuất được tiếng.
- Đồng hồ audio và API kéo mẫu, không phụ thuộc thiết bị.

Không có: phát ra loa, keyframe âm lượng, hiệu ứng audio (EQ, limiter), đổi tốc độ, kênh ngoài stereo ở đầu ra, chỉnh độ lệch tiếng và hình bằng tay.

## Project JSON

Phiên bản 2 thêm các trường tuỳ chọn. Engine vẫn đọc được phiên bản 1. Không có `output.audio` thì file xuất không có luồng tiếng, nên project cũ cho kết quả như trước.

```json
{
  "version": 2,
  "output": {
    "width": 1920, "height": 1080, "frame_duration": 3000,
    "audio": { "sample_rate": 48000, "channels": 2, "bitrate": 192000 }
  },
  "tracks": [
    { "clips": [ { "source": "a.mp4", "start": 0, "source_in": 0, "duration": 450000, "gain_db": -3.0, "fade_out": 45000 } ] },
    { "kind": "audio", "clips": [ { "source": "music.m4a", "start": 0, "source_in": 0, "duration": 450000, "fade_in": 90000 } ] }
  ]
}
```

Kiểm hợp lệ thêm: `sample_rate` khác 48000, `channels` khác 2, `gain_db` không hữu hạn, fade âm, tổng `fade_in` và `fade_out` dài hơn clip, track audio có transition hoặc effect, nguồn của track audio không có luồng tiếng. Clip video mà nguồn không có tiếng thì coi là im lặng, không báo lỗi.

## Thời gian

Vị trí mẫu là `floor(t * 8 / 15)` với `t` tính bằng tick, chỉ dùng số nguyên. Cả đầu và cuối clip đều làm tròn cùng một cách, nên hai clip liền nhau không hở và không chồng mẫu. Mọi vị trí mẫu là `i64`. Việc này không phụ thuộc vấn đề 23,976 fps đang ghi ở backlog, vì mẫu được tính từ tick chứ không từ số khung.

## Luồng

1. Scheduler audio tính các đoạn nguồn cho một khoảng mẫu, độ lợi đầu và cuối của từng đoạn. Phần này là hàm thuần trong crate `engine`.
2. Decoder audio của backend tua, giải mã, resample bằng libswresample và trả đúng số mẫu yêu cầu. Nguồn hết tiếng sớm thì phần còn lại là im lặng.
3. Bộ trộn cộng các đoạn, áp độ lợi rồi kẹp giá trị.
4. Vòng render, sau mỗi khung video, trộn tiếng tới hết thời lượng khung đó rồi đưa cho encoder. Muxer xen kẽ hai luồng.

Vị trí tiếng trong nguồn phải tính cùng gốc với video: `source_in` cộng độ lệch giữa `start_time` của luồng tiếng và luồng hình. Tua xong phải bỏ mẫu thừa để tới đúng vị trí, không nhận gói đầu tiên như đang có.

Đồng hồ audio tính `Time` từ số mẫu đã được thiết bị tiêu thụ. Khi tua thì đặt lại vị trí và bỏ các mẫu đã chuẩn bị.

## Thứ tự làm

Mỗi bước là một commit, build và test xong mới sang bước sau.

1. Model, phiên bản 2, kiểm hợp lệ.
2. Scheduler audio và bộ trộn thuần, dùng nguồn giả.
3. Decoder audio có resample.
4. Encoder AAC, mux hai luồng, nối vào vòng render và CLI.
5. Đồng hồ audio và API kéo mẫu.

## Kiểm thử và tiêu chí xong

| Tiêu chí | Cách kiểm |
|---|---|
| Không lệch mẫu | Test số nguyên cho cắt, dịch clip, nhịp 30000/1001 qua 10 phút; tổng số mẫu bằng độ dài timeline |
| Project cũ không đổi | Project phiên bản 1 cho cùng hash khung và không có luồng tiếng |
| Trộn đúng | Nguồn giả có giá trị biết trước; kiểm gain, fade, crossfade, kẹp, im lặng ở khoảng trống |
| Đúng vị trí nguồn | Clip sóng sin đổi tần số theo từng giây, kiểm tần số sau khi cắt và tua, kể cả nguồn 44,1 kHz và mono |
| Lặp lại trùng khớp | Hash PCM trước encoder qua hai lần render trên cùng máy |
| Đồng bộ tiếng hình | Clip có khung sáng và tiếng bíp cùng lúc; đo độ lệch ở file xuất, dưới 1 khung |
| Xuất file | ffprobe xác nhận có AAC, 48 kHz, stereo, thời lượng tiếng lệch hình dưới 1 khung AAC cộng phần priming |
| Đồng hồ | Thiết bị giả tiêu thụ theo từng khối; kiểm vị trí, tua và thiếu dữ liệu |
| Từ chối project sai | Test từng lỗi hợp lệ nêu trên |

AAC mất dữ liệu nên hash chỉ so ở PCM trước encoder.

## Phụ thuộc và rủi ro

- Backend mới liên kết `avcodec`, `avformat`, `avutil`. Cần thêm `swresample` vào liên kết và binding FFI. DLL và thư viện đã có trong `third_party/ffmpeg`.
- Encoder hiện chỉ có một luồng video, phải sửa để quản hai luồng. Lỗi ở đây dễ làm hỏng đường xuất đã có test.
- AAC có độ trễ đầu (priming). Phải kiểm MP4 ghi đúng để tiếng không lệch hình; chưa thử với FFmpeg bản đang dùng.
- Kết quả số thực chỉ cam kết trùng trên cùng máy và cùng bản build. Hàm sin/cos của crossfade có thể khác nhau giữa các nền tảng.
- Tua nguồn audio nén có thể không chính xác theo mẫu, nhất là MP3 và nguồn có nhiều độ trễ đầu. Các test đầu chỉ dùng AAC trong MP4 và WAV.
