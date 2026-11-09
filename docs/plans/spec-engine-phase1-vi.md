# Đặc tả giai đoạn 1: engine không UI

Ngày lập: 2026-11-09. Trạng thái: **đề xuất, chưa code**. Kế hoạch tổng ở [plan-video-editor-vi.md](plan-video-editor-vi.md), kết quả POC ở [result-poc-phase0-vi.md](result-poc-phase0-vi.md).

Mục tiêu: từ một file project JSON, CLI xuất ra file video, và render lại cho kết quả trùng khớp để làm test hồi quy.

## Phạm vi

Có:

- Crate `engine` (thư viện) và `gpufx-render` (CLI). Crate `poc` giữ nguyên để đo so sánh.
- Thời gian số nguyên, project JSON có kiểm hợp lệ.
- Nhiều track video, cắt clip, transition giữa hai clip liền nhau, hiệu ứng theo clip với tham số tĩnh.
- Đưa mọi clip về kích thước project trước khi ghép.
- Xuất H.264 bằng x264 khi có FFmpeg GPL, hoặc encoder phần cứng.
- Bộ test tự động của workspace.

Không có: audio, UI, keyframe, text, proxy, cache đĩa, HDR, phục hồi khi effect lỗi.

Linux và web chưa làm ở giai đoạn này, nhưng code decode và GPU phải nằm sau interface để thêm backend Vulkan mà không sửa project model hay scheduler.

## Thời gian

`Time` là `i64`, đơn vị 1/90000 giây. Đơn vị này biểu diễn chính xác 24, 25, 30, 60 fps và các nhịp 1000/1001. Mọi phép tính trên timeline dùng số nguyên, không dùng số thực.

## Project JSON

```json
{
  "version": 1,
  "output": { "width": 1920, "height": 1080, "frame_duration": 3000 },
  "tracks": [
    {
      "clips": [
        { "source": "a.mp4", "start": 0, "source_in": 90000, "duration": 450000, "effects": [] }
      ],
      "transitions": [
        { "after_clip": 0, "package": "mix", "duration": 45000, "params": {} }
      ]
    }
  ]
}
```

Project không hợp lệ phải bị từ chối với thông báo chỉ rõ vị trí: clip chồng nhau trong track, transition dài hơn phần clip có thể dùng, nguồn không tồn tại, gói hiệu ứng không tìm thấy.

## Luồng render

1. Scheduler tính danh sách layer và thời điểm nguồn cho từng khung của timeline.
2. Decoder trả khung theo thứ tự cần, chỉ tua khi không đi tiếp được.
3. Compositor đưa khung về kích thước project, chạy transition và effect, ghép các track từ dưới lên.
4. Encoder nhận khung kết quả và ghi file.

Giữ nguyên bản sửa của POC: chờ decoder rảnh trước khi khung vào gpufx.

## Kiểm thử và tiêu chí xong

| Tiêu chí | Cách kiểm |
|---|---|
| Render lặp lại trùng khớp | Render hai lần trên cùng máy và driver, hash RGBA từng khung phải bằng nhau |
| Đúng thời gian | Test số nguyên cho cắt, dịch clip, nhịp 30000/1001 qua 10 phút, không lệch khung |
| Đúng khung | Clip có số khung vẽ sẵn, kiểm khung xuất ra đúng nguồn ở đầu, cuối clip và trong transition |
| Từ chối project sai | Test từng lỗi hợp lệ nêu trên |
| Xuất file | ffprobe xác nhận số khung, kích thước, codec; file phát được |

Hash chỉ so trên cùng máy và driver, vì GPU khác có thể lệch nhỏ. Test cần GPU đánh dấu riêng để máy CI không có GPU bỏ qua được.

## Phụ thuộc và rủi ro

- FFmpeg GPL: release hiện chỉ có bản LGPL. Engine dựng và test được với bản LGPL vì cùng API, x264 chỉ cần khi xuất. Cần một bản GPL cùng commit trước khi test xuất x264.
- `GpuContext::from_device` và zero-copy cho texture 1920x1088 đang chờ phía gpufx. Engine dùng đường vòng của POC cho tới khi có.
- Clip GOP dài tua chậm (p95 502 ms ở POC). Render tuần tự ít bị ảnh hưởng; preview xử lý ở giai đoạn proxy.
