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

- File project tên `<tên>.json.gpufxproj` (bên trong là JSON); `<tên project>` dưới đây là phần trước đuôi kép đó. Hằng số và hàm tách tên ở `crates/gpufx-editor/src/project_zip.rs` (`PROJECT_EXTENSION`, `ARCHIVE_EXTENSION`, `project_stem`). Hộp thoại Lưu tự thêm đuôi khi người dùng gõ tên thiếu đuôi.
- Menu File chỉ có "Lưu" và "Lưu thành...". Trên Windows hộp thoại Lưu là `IFileSaveDialog` của shell kèm hai ô đánh dấu (`crates/gpufx-editor/src/save_dialog.rs`): "Lưu kèm gói và tệp media" (mặc định đánh dấu khi project đang mở đã có thư mục `<tên project>.bundle`) và "Đóng gói zip". Đánh dấu zip thì luôn tạo bundle. Ngoài Windows dùng `rfd`, không có hai ô này.
- Lưu kèm (ô thứ nhất hoặc ô zip, hoặc mọi lần lưu khi đã có thư mục `<tên project>.bundle` cạnh project, kể cả lệnh "Lưu" không mở hộp thoại) thì mỗi lần lưu project chép các gói project dùng vào `<thư mục project>/<tên project>.bundle/packages/<khoá>`, tức thư mục `packages` có dạng một gốc gói. Mọi tệp project đọc (video, âm thanh, ảnh, font, texture chữ, media của motion track) cũng được đưa vào `<tên project>.bundle/media/{video,audio,images,fonts,subtitles,other}/`, giữ tên gốc (trùng tên với tệp khác thì thêm ` (2)`, ` (3)`…), cùng ổ thì tạo hard link, khác ổ thì chép giữ thời gian sửa; bản đã khớp kích thước và thời gian sửa thì bỏ qua. Sau đó project trỏ vào các bản trong bundle (lưu dạng tương đối). Tệp thiếu được báo, không chặn việc lưu. Mã ở `crates/gpufx-editor/src/project_bundle.rs`. Chép vào thư mục tạm rồi đổi tên, thay bản cũ chỉ khi bản mới đã chép xong. Lỗi chép không làm hỏng việc lưu project, chỉ báo lỗi.
- Đóng gói zip: sau khi lưu JSON và bundle, ghi `<tên project>.zip.gpufxproj` cạnh project (bên trong là zip thường) chứa file project và cả cây `<tên project>.bundle/`, đường dẫn tương đối dùng `/`, bỏ qua thư mục tạm `.copying`. Ghi ra file `.partial` rồi đổi tên. Media đã nén (mp4, mov, mkv, webm, mp3, aac, m4a, jpg, png, webp, zip…) lưu dạng Stored, còn lại deflate. Lỗi tạo zip không làm hỏng việc lưu, chỉ báo lỗi.
- Mở: hộp thoại Mở nhận `*.gpufxproj`, `*.json` và `*.zip`. File đuôi `.zip.gpufxproj`, `.zip`, hoặc `*.gpufxproj` khác bắt đầu bằng `PK` được giải nén (trên luồng đọc project) vào thư mục `<tên project>` cạnh nó; thư mục đó đã có và không rỗng thì dùng `<tên project> (2)`, `(3)`… Mục zip có đường dẫn tuyệt đối, ổ đĩa hoặc `..` làm hỏng cả lần giải nén. Sau đó mở file project duy nhất ở gốc thư mục (không có hoặc có nhiều hơn một thì báo lỗi).
- Khi đường dẫn project đổi (mở, lưu thành), editor tìm các gói project dùng mà không gốc nào có (kể cả gói đang tắt) nhưng bản sao có; nếu có thì mở cửa sổ quản lý gói với dòng đề nghị "Cài từ bản sao". Cài là chép từ bản sao vào cùng khoá dưới gốc user, giữ nguyên gói user đã có.
- Khoá lấy từ project được kiểm từng phần (không `..`, không ổ đĩa, tên hợp lệ trên Windows) trước khi ghép đường dẫn.

## Giao diện

- Nút mở: menu File > "Quản lý gói...". Cửa sổ egui `Window` không modal, giữ theme hiện tại.
- Tab: Hiệu ứng, Chuyển cảnh, Kiểu chữ, Kiểu phụ đề, Animation, Sticker, Hiệu ứng âm thanh, Nhạc, Font. Cột: bật, tên, phiên bản, nguồn, số mục, dung lượng, nút Mở thư mục và Gỡ. Ô lọc theo tên, id, phiên bản.
- Danh sách được đọc trên luồng riêng vì đọc gói shader phải mở gói.

## MCP

- Tool `list_packages` chỉ đọc, tham số `kind` tuỳ chọn; trả các trường như cửa sổ. Cài/gỡ qua MCP để sau.
