# Quản lý gói

Ghi lại thiết kế cửa sổ quản lý gói và các quyết định đi kèm. Mã ở `crates/gpufx-editor/src/package_manager.rs` (logic), `crates/gpufx-editor/src/ui/package_manager.rs` (cửa sổ) và `crates/d3d12-backend/src/paths.rs` (gói tắt).

## Liệt kê

- Mỗi type giữ module cài riêng (`*_install.rs`, `packages.rs` cho gói shader). Mỗi module có `list_installed(root)` đọc manifest bằng parser và bộ kiểm tra sẵn có của nó; `package_manager::list_installed` chỉ gom lại. User đã chốt không gom các module cài thành một module chung.
- Khoá của gói là đường dẫn dưới `packages/`, chữ thường: `sticker/my-pack`, `gpufx/ffmpeg/box-blur`, `gpufx/user/io.example.iris.zip`.
- Nguồn: gốc user (`%LOCALAPPDATA%\gpufx-editor`) là "Đã cài", mọi gốc khác là "Kèm app" và chỉ đọc. Bản debug có `target\debug\packages` thì đó là gốc kèm app, gốc user vẫn cài/gỡ được.
- Gói hỏng (manifest không đọc được, sai id thư mục, gói shader cần số input không dùng được) vẫn hiện, tên đỏ kèm lý do, và gỡ được nếu ở gốc user.
- Hai gốc có cùng khoá: gói ở gốc tìm trước được dùng, gói sau hiện ghi chú "không dùng".

## Bật/tắt

- Danh sách gói tắt nằm ở `packages/disabled.json` của gốc user (mảng khoá). Đọc từ mọi gốc, chỉ ghi vào gốc user.
- Gói tắt không được nạp ở đâu cả: mọi hàm quét thư viện bỏ qua nó, `ProjectPaths` không phân giải gói shader tắt (`package_path` trả `None`) và trả `AssetProblem::PackDisabled` cho asset của gói tắt. Editor, MCP và export đều đi qua các đường đó.
- Gỡ một gói thì xoá khoá của nó khỏi danh sách, để cài lại thì gói bật sẵn.

## Gỡ

- Chỉ gỡ được gói ở gốc user. Giữ khoá cài (`lock_installs`), đổi tên gói sang `packages/.remove-tmp/<duy nhất>` rồi mới xoá, nên gói rời chỗ ngay và nguyên vẹn; phần chưa xoá được (file đang mở) được dọn ở lần gỡ sau. Không dùng `.install-old` vì bước phục hồi của lần cài sẽ trả gói ở đó về chỗ cũ.
- Trước khi gỡ, cửa sổ liệt kê các clip và chuyển cảnh của project đang mở dùng gói (effect, chuyển cảnh, nguồn `asset:`, font của gói, animation `<gói>/<item>`) rồi vẫn cho gỡ. Kiểu chữ và kiểu phụ đề được chép vào clip nên không tính.
- Sau mỗi lần cài, gỡ, bật/tắt, khôi phục: quét lại mọi thư viện (gồm font), dựng lại `ProjectPaths` và `font_paths`, vẽ lại preview, đọc lại danh sách gói.

## Gói nội bộ

- Manifest gói shader có `"hidden": true` (trường mới trong `gpufx-core`, nhánh `feat/hidden-effects` của FFmpegGpuFilter): gói vẫn ở catalog nên project đã gắn và animation vẫn render, nhưng `filter_entries` bỏ nó khỏi lưới hiệu ứng và khỏi `packages_list` của MCP. Cửa sổ quản lý gói hiện nó với nhãn "nội bộ".

## Lưu kèm bản sao gói

- Tuỳ chọn ở menu File, lưu trong settings của máy (`save_with_packs`), mặc định tắt.
- Bật thì mỗi lần lưu project chép các gói project dùng vào `<thư mục project>/<tên project>.packages/packages/<khoá>`, tức thư mục đó có dạng một gốc gói. Chép vào thư mục tạm rồi đổi tên, thay bản cũ chỉ khi bản mới đã chép xong. Lỗi chép không làm hỏng việc lưu project, chỉ báo lỗi.
- Khi đường dẫn project đổi (mở, lưu thành), editor tìm các gói project dùng mà không gốc nào có (kể cả gói đang tắt) nhưng bản sao có; nếu có thì mở cửa sổ quản lý gói với dòng đề nghị "Cài từ bản sao". Cài là chép từ bản sao vào cùng khoá dưới gốc user, giữ nguyên gói user đã có.
- Khoá lấy từ project được kiểm từng phần (không `..`, không ổ đĩa, tên hợp lệ trên Windows) trước khi ghép đường dẫn.

## Giao diện

- Nút mở: menu File > "Quản lý gói...", cạnh tuỳ chọn lưu kèm bản sao. Cửa sổ egui `Window` không modal, giữ theme hiện tại.
- Tab: Hiệu ứng, Chuyển cảnh, Kiểu chữ, Kiểu phụ đề, Animation, Sticker, Hiệu ứng âm thanh, Nhạc, Font. Cột: bật, tên, phiên bản, nguồn, số mục, dung lượng, nút Mở thư mục và Gỡ. Ô lọc theo tên, id, phiên bản.
- Danh sách được đọc trên luồng riêng vì đọc gói shader phải mở gói.

## MCP

- Tool `list_packages` chỉ đọc, tham số `kind` tuỳ chọn; trả các trường như cửa sổ. Cài/gỡ qua MCP để sau.
