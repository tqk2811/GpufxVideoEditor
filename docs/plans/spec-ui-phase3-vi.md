# Đặc tả giai đoạn 3: UI tối thiểu

Ngày lập: 2026-11-09. Trạng thái: **đã code, chờ thao tác tay; bảng quyết định vẫn là đề xuất tạm**. Kế hoạch tổng ở [plan-video-editor-vi.md](plan-video-editor-vi.md), audio ở [spec-audio-phase2-vi.md](spec-audio-phase2-vi.md).

Mục tiêu: một ứng dụng desktop Windows để cắt, kéo clip trên nhiều track, chọn hiệu ứng, undo, xem preview có tiếng và xuất file.

## Quyết định cần chốt

Các mục dưới đây là đề xuất, chỉ user chốt được.

| Vấn đề | Đề xuất | Lý do |
|---|---|---|
| Crate | `gpufx-editor` mới, dùng egui 0.37 như POC | Dùng chung wgpu device, preview không qua interop |
| Undo | Lưu ảnh chụp `Project` trước mỗi lệnh | Project nhỏ, đơn giản và khó sai hơn lệnh đảo ngược |
| Kéo clip chồng lên clip khác | Từ chối, clip về vị trí hợp lệ gần nhất, không đẩy clip khác | Đúng luật không chồng clip hiện có; ripple làm sau |
| Phát tiếng ra loa | `cpal` (WASAPI trên Windows) | Có sẵn cả Linux cho giai đoạn sau |
| Hộp thoại file | `rfd` | Dùng hộp thoại của hệ điều hành |
| Tham số hiệu ứng | Chỉ chọn gói và dùng tham số mặc định | Keyframe và inspector đầy đủ thuộc giai đoạn 4 |

## Phạm vi

Có:

- Timeline nhiều track: thêm track, thêm clip từ file, kéo clip trong và giữa các track cùng loại, cắt đầu, cắt cuối, tách tại đầu phát, xoá clip.
- Undo và redo cho mọi thao tác sửa timeline.
- Danh sách hiệu ứng và transition đọc từ manifest của các gói.
- Preview trên cùng wgpu device, phát có tiếng, audio làm đồng hồ chính, tua bằng cách kéo đầu phát.
- Lưu và mở project JSON, xuất file chạy nền có tiến độ và huỷ được.

Không có: keyframe, text, proxy, cache đĩa, phục hồi khi effect lỗi, Linux, chỉnh tham số hiệu ứng, ripple edit.

## Thao tác

| Thao tác | Cách làm |
|---|---|
| Phát hoặc dừng | Space |
| Tách clip tại đầu phát | S |
| Xoá clip đang chọn | Delete |
| Undo, redo | Ctrl+Z, Ctrl+Y |
| Cuộn ngang timeline | Shift + lăn chuột, chỉ nhận khi timeline còn chỗ để cuộn |
| Zoom timeline quanh con trỏ | Ctrl + lăn chuột |

## Kiến trúc

Mọi thao tác sửa là một lệnh thuần trong crate `engine`, nhận `Project` và trả `Project` mới hoặc lỗi. UI chỉ phát lệnh. Việc đổi toạ độ màn hình sang `Time`, hit-test và tính vị trí thả là hàm thuần để test không cần cửa sổ.

Giải mã và ghép khung preview chạy trên một luồng riêng, giữ quy tắc tuần tự hoá mọi `submit` trên device. Luồng UI nhận khung mới nhất và không chờ decoder.

Đường tiếng tách làm hai đầu theo mục backlog về `AudioClock`: luồng chuẩn bị giải mã và trộn rồi ghi vào ring buffer một ghi một đọc, callback của thiết bị chỉ đọc, không khoá và không cấp phát. Timeline đổi thì đồng hồ được dựng lại từ project mới.

Khi đang phát, khung hình theo vị trí đồng hồ audio. Khung tới muộn thì bỏ, không chờ.

## Thứ tự làm

Mỗi bước một commit, build và test xong mới sang bước sau.

1. Lệnh sửa timeline và undo trong `engine`.
2. Tách đường tiếng thành ring buffer, đóng mục backlog về `AudioClock`.
3. Crate `gpufx-editor`: cửa sổ, vẽ timeline, chọn, kéo, cắt, zoom, cuộn, phím tắt.
4. Preview khung hình và tua.
5. Phát tiếng qua `cpal`, đồng bộ hình theo tiếng.
6. Chọn hiệu ứng từ manifest, lưu và mở project, xuất nền.

## Kiểm thử và tiêu chí xong

| Tiêu chí | Cách kiểm |
|---|---|
| Lệnh sửa đúng | Test từng lệnh trên project mẫu; project kết quả qua `validate` |
| Undo đúng | Chuỗi lệnh rồi undo hết bằng project ban đầu; redo hết bằng project cuối |
| Tách không lệch | Hai nửa sau khi tách có cùng khung và mẫu tại mọi vị trí so với clip gốc |
| Toạ độ đúng | Test đổi điểm ảnh và `Time` ở nhiều mức zoom, zoom giữ nguyên thời điểm dưới con trỏ |
| Ring buffer an toàn | Test thứ tự mẫu và thiếu dữ liệu với hai luồng; tua không phát lại mẫu cũ |
| Xuất từ UI trùng CLI | Cùng project cho cùng hash khung và PCM |
| Thao tác tay | Chạy app thật theo danh sách kiểm: thêm clip, kéo, cắt, undo, phát, tua, xuất |

Âm thanh ra loa và độ mượt khi phát phải có người nghe và nhìn thử, test tự động không thay được.

## Phụ thuộc và rủi ro

- Clip GOP dài tua chậm (p95 502 ms ở POC), nên kéo đầu phát sẽ trễ tới khi có proxy.
- `process_d3d12` và giải mã dùng chung device, nên luồng preview phải là nơi duy nhất submit công việc ghép.
- Xuất nền chạy cùng lúc với preview sẽ tranh GPU và decoder. Bản đầu dừng preview khi đang xuất.
- Effect lỗi có thể làm hỏng device tới hết tiến trình. Giai đoạn này chỉ báo lỗi rõ ràng, phục hồi thuộc giai đoạn 4.
