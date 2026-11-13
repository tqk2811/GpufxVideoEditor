# Đặc tả MCP server cho agent

Ngày lập: 2026-11-09. Trạng thái: **đã code trên nhánh `feat/mcp-server`**; các mục "Đề xuất" trong bảng dưới vẫn chờ user chốt. Kế hoạch tổng ở [plan-video-editor-vi.md](plan-video-editor-vi.md), giao diện ở [spec-ui-phase3-vi.md](spec-ui-phase3-vi.md).

Mục tiêu: agent dựng video qua MCP với cùng các lệnh mà giao diện dùng, tự xem kết quả bằng ảnh chụp, và xuất file.

## Quyết định

| Vấn đề | Lựa chọn | Trạng thái |
|---|---|---|
| Chế độ chạy | Cả hai: trên project file không cần mở app, và điều khiển cửa sổ editor đang mở | Đã chốt |
| Chụp hình | Có, theo thời điểm hoặc frame index | Đã chốt |
| Crate | `gpufx-mcp` mới, một binary, giao tiếp MCP qua stdio | Đề xuất |
| Thư viện MCP | `rmcp` (SDK Rust chính thức), kiểm phiên bản khi làm | Đề xuất |
| Nối vào app đang mở | Named pipe cục bộ của user hiện tại, app bật bằng một tuỳ chọn, mặc định tắt | Đề xuất |
| Chụp khung gốc của file chưa đưa vào timeline | Chưa làm (cần thêm đường dựng project tạm theo kích thước file, ở cả hai chế độ), thêm sau nếu user cần | Đề xuất |

## Kiến trúc

Một bộ tool dùng chung cho cả hai chế độ. Tool chỉ gọi một trait `EditorSession`, nên agent không phải biết đang ở chế độ nào.

| Thành phần | Trách nhiệm |
|---|---|
| Lớp tool | Khai báo tool, kiểm tham số, đổi lỗi thành kết quả có cấu trúc |
| `EditorSession` | Đọc project, chạy lệnh sửa, undo, chụp hình, xuất file |
| `FileSession` | Giữ một `EditorState` không cửa sổ trong tiến trình MCP (chính state mà cửa sổ dùng), tự mở `Gpu` khi cần chụp |
| `LiveSession` | Gửi lệnh qua named pipe tới app đang mở |
| `gpufx_editor::agent` | Kiểu yêu cầu chung, `execute` chạy một yêu cầu trên `EditorState`, `gateway` là named pipe. Yêu cầu đi tới luồng UI bằng user event của winit, nên cửa sổ thu nhỏ vẫn nhận |

Lệnh sửa là các hàm thuần đã có trong `engine::edit`, xuất file chạy `gpufx-render` thành tiến trình con (`ExportWorker`). Hai chế độ cùng đi qua `execute`, nên trait `EditorSession` chỉ có một hàm `call`. Việc đọc đĩa (mở project, probe file nguồn, quét package) nằm ở `prepare`, chạy ngoài luồng UI. Chế độ file không dựng cửa sổ và không mở GPU cho tới tool đầu tiên cần tới nó.

Ở chế độ live, lệnh đi cùng đường với thao tác tay qua `EditorState`, nên preview, audio và undo tự cập nhật, và user thấy thay đổi ngay.

## Chống sửa nhầm clip

`ClipKey` là chỉ số và đổi sau khi sửa. Ở chế độ live, user có thể sửa xen giữa hai lần gọi của agent.

- Mọi kết quả trả về kèm `revision`.
- Tool sửa bắt buộc nhận `expected_revision`. Lệch thì trả lỗi `stale_revision` kèm revision mới, không sửa gì.
- Tool thêm hoặc tách clip trả về key của clip mới, tránh việc agent phải đoán lại chỉ số.

## Tool

Thời gian luôn là số nguyên tick 1/90000 giây, giống project JSON.

| Tool | Việc |
|---|---|
| `session_status` | Chế độ, đường dẫn project, revision, có đang xuất hoặc đang kéo không |
| `project_new`, `project_open`, `project_save` | Tạo, mở, lưu project |
| `project_get` | Trả project JSON, thời lượng và số khung |
| `packages_list` | Danh sách effect và transition, lọc theo chuỗi |
| `track_add` | Thêm track |
| `track_set_locked`, `track_set_hidden` | Khoá hoặc ẩn track. Sửa track đang khoá bị từ chối với lỗi `track_locked` |
| `clip_add`, `clip_move`, `clip_trim_start`, `clip_trim_end`, `clip_split`, `clip_delete` | Sửa clip |
| `effect_add`, `effect_remove`, `transition_add`, `transition_remove` | Hiệu ứng và transition |
| `text_add`, `text_set` | Thêm và sửa clip chữ, xem mục `Chữ` |
| `font_list`, `clip_animation_list` | Danh sách font và danh sách preset animation (id, nhóm `in`/`out`/`loop`, thời lượng mặc định) |
| `text_style_list` | Danh sách kiểu chữ và kiểu phụ đề của các gói `text-style`/`subtitle-style` đã cài, lọc theo `type` (`text`/`subtitle`) |
| `subtitle_import_srt`, `subtitle_set_style`, `subtitle_export_srt` | Nhập SRT thành track phụ đề, đổi kiểu phụ đề của track, xuất track ra SRT, xem mục `Phụ đề` |
| `clip_animation_set` | Đặt animation `in`, `out`, `loop` cho một clip có hình (video, ảnh, sticker, chữ) |
| `clip_set_gain`, `clip_set_muted`, `clip_set_fades` | Âm lượng và fade |
| `clip_set_speed` | Tốc độ phát một clip, xem mục dưới |
| `clip_set_transform` | Vị trí, độ mờ, xoay tĩnh của clip |
| `keyframe_set`, `keyframe_remove` | Keyframe cho x, y, width, opacity, rotation và tham số float của effect, thời điểm tính bằng tick từ đầu clip |
| `undo`, `redo` | Hoàn tác, mỗi tool sửa là một bước; `track_set_locked` không phải một bước, giống nút khoá |
| `frame_capture` | Chụp khung đã ghép |
| `export_start`, `export_status`, `export_cancel` | Xuất nền |

### `frame_capture`

Nhận đúng một trong hai tham số `time` (tick) hoặc `frame` (index), và `max_width` tuỳ chọn. Có cả hai hoặc thiếu cả hai là lỗi tham số. Vị trí ngoài timeline là lỗi, không chụp khung gần nhất.

Trả về ảnh PNG dạng nội dung image của MCP, cùng frame index, thời điểm thực của khung và revision. Khung đã gồm hiệu ứng và transition. `time` được đổi sang frame index bằng đúng công thức preview dùng. Ảnh được thu nhỏ khi quá `max_width`, mặc định 1280 để không làm phình ngữ cảnh agent.

Chế độ file dựng khung qua `FrameRenderer` trên device riêng. Chế độ live chạy việc chụp trên luồng preview, xen giữa các khung (`PreviewWorker::run_on_thread`), không thay yêu cầu preview đang chờ, không báo khung đang dựng là hết cần. Chụp dùng decoder riêng và cờ abort tắt, nên không di chuyển đầu phát, không thay khung đang hiển thị và không bị yêu cầu preview mới làm hỏng. Trong lúc xuất, chụp trả `busy` ở cả hai chế độ.

### `clip_set_speed` và `speed` của `clip_add`

`clip_set_speed` nhận `clip`, `speed` (từ 0,1 đến 10, ví dụ 2 là nhanh gấp đôi) và `expected_revision`. Nó giữ nguyên đoạn nguồn đang phát nên thời lượng trên timeline thành `đoạn nguồn / speed`, tính từ điểm đầu cũ. Lỗi: `speed_overlap` khi clip sẽ chồng lên clip kế tiếp (không tự đẩy clip khác), `transition_on_edge` khi đuôi clip đang có transition, `out_of_range` khi tốc độ ngoài khoảng, `invalid_project` khi fade hoặc transition đầu clip không còn vừa, `track_locked` trên track khoá. `clip_add` nhận thêm `speed` tuỳ chọn; thời lượng mặc định là phần nguồn còn lại chia cho `speed`. `duration` truyền tay cùng `speed` bị từ chối bằng `out_of_range` khi `source_in + duration * speed` vượt quá độ dài của stream đã biết (áp dụng cả khi `speed` là 1). `project_get` trả `speed` khi khác 1.

`keyframe_set` và `keyframe_remove` nhận `clip`, `at`, `expected_revision`, và hoặc `property` (`x`, `y`, `width`, `opacity`, `rotation`) hoặc `effect` (chỉ số) cùng `param` (tham số float của effect đó). `at` là số tick từ `clip.start`, KHÁC với `clip_time` mà gói effect đọc, vốn đếm từ đầu cửa sổ transition đi vào. Phạm vi hợp lệ khi đặt là [0, `duration`): đúng `duration` bị từ chối vì không có khung hình nào ở đó. `keyframe_set` thay keyframe cùng tick, `easing` vắng thì giữ easing cũ; giá trị ngoài giới hạn của thuộc tính hoặc ngoài `min`/`max` của tham số bị từ chối. Mã lỗi: `keyframe_outside_clip` khi `at` ngoài [0, `duration`) lúc đặt, `keyframe_not_found` khi `keyframe_remove` không có keyframe đúng tick `at`. `keyframe_remove` xoá được cả keyframe nằm ngoài clip (do cắt đầu hoặc đuôi clip để lại); khi xoá xong mà thuộc tính không còn keyframe nào trong [0, `duration`), các keyframe còn lại của nó bị xoá theo và thuộc tính về giá trị tĩnh. `project_get` trả `motion` của mỗi clip (`opacity`, `rotation` tĩnh và `keyframes` theo thuộc tính) cùng `keyframes` của từng effect; clip không dùng thì hai trường này vắng.

### Chữ

Clip chữ là clip có `source` `text:` và trường `text` (`TextContent`); `project_get` trả cả trường này nên agent đọc được nội dung và kiểu chữ qua tool đó, không cần tool đọc riêng. Mọi tool sửa đi qua `engine::add_text_clip` và `set_text`, nên mỗi lần là một bước undo.

`text_add` nhận `start`, `duration` (tick), `content`, `expected_revision`, và tuỳ chọn `track`, `style`. `track` vắng thì dùng track video đầu tiên còn chỗ tại `start`, không có thì thêm track mới, giống nút "Thêm chữ". `style` có các trường `font`, `size`, `color`, `outline`, `background`, `shadow`, `align`, `line_spacing`, `animation`, tên và nghĩa giống `TextContent` trong project; trường vắng lấy giá trị của nút "Thêm chữ" (trắng, canh giữa, font mặc định của hệ thống (Segoe UI, không có thì Arial; editor không kèm font nào), `size` 0,08 phần chiều cao khung). Clip được đặt ở cỡ tự nhiên giữa khung; trả về `clip`.

`text_set` nhận `clip`, `expected_revision`, `content` và các trường của `style`, ghi phẳng cùng cấp. Trường vắng giữ nguyên; `null` ở `outline`, `background`, `shadow` hoặc một nhóm của `animation` (`in`, `out`, `loop`) thì gỡ phần đó. Đổi `size` hoặc `content` thì vị trí của clip co giãn quanh tâm cũ như khi sửa trong inspector (`rescaled_placement`), clip có keyframe vị trí thì giữ nguyên vị trí. Clip không phải chữ bị từ chối với `not_text`.

`font_list` trả `{state, fonts}`. Mỗi phần tử của `fonts` là giá trị dùng được ngay cho `font`: `{"kind": "default"}` đứng đầu, rồi `{"kind": "system", "family": ...}`. Lần quét font hệ thống chưa xong thì `state` là `loading` và chỉ có font mặc định; gọi lại sau, `state` thành `ready`. Khi đã `ready`, một family không có trong danh sách bị từ chối; còn `loading` thì tên được nhận như đã cho. `font` còn nhận `{"kind": "file", "path": ...}` trỏ tới một file font (`.ttf`, `.otf`, `.ttc`, dùng face đầu tiên): `path` phải là đường dẫn tuyệt đối trên ổ đĩa của máy (không nhận share mạng `\\server\share`, `//server/share`, `\\?\UNC\...` hay thiết bị `\\.\...`), tới file có thật, không quá 50 MiB (kiểm trước khi đọc) và đọc được là font, sai thì `invalid_params`. File này không nằm trong `font_list`. Khi lưu project, đường dẫn nằm trong thư mục project được ghi tương đối như nguồn media.

`text_add` (trong `style`) và `text_set` nhận thêm `style_id` dạng `"<pack>/<item>"` do `text_style_list` liệt kê: giá trị của kiểu được chép vào chữ trước (giữ `content`; kiểu không có `size` thì giữ cỡ cũ; font lấy họ đầu tiên trong `font_hint` mà máy có, không có thì giữ font cũ), animation của kiểu (nếu có) thay cả ba nhóm, kiểu phụ đề đặt chữ theo `layout`; các trường khác trong cùng lời gọi ghi đè lên kiểu. Kiểu không có thì `invalid_params`. Project không lưu id kiểu.

`text_style_list` trả `{"state", "styles": [{"id", "name", "type", "category", "preview_text", "pack", "style", "animation", "layout"}, ...], "problems"}`; `state` là `loading` khi các gói còn đang được đọc.

`clip_animation_list` trả `{"presets": [{"id", "group", "default_duration"}, ...]}`, `group` là `in`, `out` hoặc `loop`, `default_duration` tính bằng tick (với `loop` là một chu kỳ). Preset chỉ được dùng đúng nhóm của nó, `duration` của một preset tính bằng tick và phải lớn hơn 0.

`clip_animation_set` nhận `clip`, `expected_revision` và các nhóm `in`, `out`, `loop`, mỗi nhóm là `{preset, duration}`. Nhóm vắng giữ nguyên, `null` gỡ animation của nhóm đó, không có nhóm nào thì `invalid_params`. Dùng được cho mọi clip có hình (video, ảnh, sticker, chữ); clip trên track audio bị từ chối với `invalid_params`. `text_add` và `text_set` vẫn nhận `animation` như cũ.

Giá trị không hợp lệ bị từ chối với `invalid_params` và nói rõ trường nào: `content` rỗng, `size` hay `line_spacing` không lớn hơn 0, độ rộng viền, `padding`, `radius` hay `blur` âm, preset không có hoặc sai nhóm, `duration` không dương, `text_set` không có trường nào để đổi.

### Phụ đề

Track phụ đề là track video có trường `subtitle: {"style"?}` trong project, mỗi cue một clip chữ (bắt đầu và kết thúc theo cue, `content` là chữ của cue). Project cũ không có trường này vẫn mở được. Kiểu phụ đề được chép vào từng clip như `style_id` của `text_add`; `subtitle.style` chỉ ghi lại id kiểu track được áp lần cuối, nên thiếu gói thì project vẫn mở như cũ. Clip có `text.own_style: true` là cue đã được áp kiểu riêng (`text_set` với `style_id`, hoặc "Áp kiểu" trong editor) và giữ kiểu đó khi track đổi kiểu.

- `subtitle_import_srt` nhận `expected_revision`, đúng một trong `path` (file UTF-8, có hoặc không BOM) và `content`, tuỳ chọn `style_id` (kiểu phụ đề do `text_style_list` liệt kê). Trình đọc chấp nhận CRLF hoặc LF, `,` hoặc `.` trước mili giây, số thứ tự thiếu hoặc sai, cue nhiều dòng; bỏ tag `<i>`, `<b>`, `<u>`, `<s>`, `<font>` và `{\an8}`. Cue không đọc được, kết thúc không sau lúc bắt đầu, hoặc không còn chữ thì bị bỏ; cue chồng lên cue sau bị cắt tại lúc cue sau bắt đầu; cue bắt đầu cùng lúc với cue trước bị bỏ. Trả `{"track", "cues", "warnings": [{"skipped", "message"}]}`, `message` có số dòng. Không còn cue nào thì `invalid_params` (kèm `data.warnings`). Không có `style_id` thì cue là chữ mặc định, đặt gần đáy khung. Một bước undo.
- `subtitle_set_style` nhận `expected_revision`, `track`, `style_id`, `all` (mặc định `false`). Chép kiểu vào mọi cue (giữ chữ và thời gian), đặt cue theo `layout` trừ cue có keyframe vị trí; cue `own_style` được giữ trừ khi `all`, khi đó cue nhận kiểu track và bỏ cờ. Track không phải track phụ đề hoặc kiểu không phải kiểu phụ đề thì `invalid_params`. Trả `{"track", "restyled"}`. Một bước undo.
- `subtitle_export_srt` nhận `track`, tuỳ chọn `path`, `overwrite`. Trả `{"track", "cues", "srt", "path"}`; `srt` dùng CRLF và `,` trước mili giây. `path` phải có đuôi `.srt` (sai thì `invalid_params`); đường dẫn tương đối tính từ thư mục chứa file project đã lưu, project chưa lưu thì bắt buộc đường dẫn tuyệt đối (`invalid_params`). File đã có thì `file_exists` trừ khi `overwrite`; file project đang đọc thì `invalid_output`.

Karaoke: `text_add` (trong `style`) và `text_set` nhận `karaoke: {mode?, active?, spoken?, upcoming?, emphasis?, words?}`, `null` thì tắt. `mode` là `none`, `color` (mỗi từ lấy màu và độ mờ của trạng thái), `box` (như `color`, thêm hộp nền sau từ đang nói, màu `active.background`, không có thì hộp vàng mặc định `[255, 200, 0, 255]`, không lấy màu chữ vì sẽ che mất từ) hoặc `scale` (chưa vẽ phóng to, vẽ như `color`). Mỗi trạng thái là `{color?, background?, scale?, opacity?}`; trạng thái nào được gửi thì thay cả trạng thái đó, trường vắng giữ nguyên. Từ là chuỗi không có khoảng trắng của chữ sau khi đổi hoa/thường (`transform`). `words` là thời gian từng từ `[{start, end, emphasis?}]` tính bằng tick theo thời gian nguồn của clip (0 là đầu cue), đúng một mục mỗi từ, theo thứ tự và không chồng nhau, sai thì `invalid_params`; danh sách rỗng hoặc không có thì thời lượng cue chia cho các từ theo số ký tự (SRT không có thời gian từng từ). Đổi `content` mà không gửi `words` thì thời gian cũ bị bỏ. Áp kiểu phụ đề có `karaoke` thì chép mục đó vào chữ (giữ `words` đã đặt); kiểu phụ đề không có `karaoke` thì tắt, kiểu chữ thường thì giữ nguyên.

### Xuất file

`export_start` trả ngay một mã công việc, `export_status` trả tiến độ, `export_cancel` huỷ. Ghi đè file có sẵn phải có `overwrite: true`. Mỗi lúc chỉ một lần xuất. Không sửa được trong lúc xuất, ở cả hai chế độ; ở chế độ live preview cũng tạm dừng, giống thao tác từ giao diện.

`export_start`, `export_status` và `export_cancel` chạy `gpufx-render --progress json` thành tiến trình con, giống hệt cách editor đang làm, và dùng lại giao thức dưới đây.

### Giao thức tiến độ

`gpufx-render <project.json> --out <file> --progress json` ghi mỗi sự kiện thành một dòng JSON trên stdout. Mặc định (`--progress text`) vẫn in văn bản cho người đọc như cũ. Kiểu dữ liệu nằm ở `crates/d3d12-backend/src/progress.rs`.

| `event` | Trường khác | Ý nghĩa |
|---|---|---|
| `started` | `protocol` (hiện là 1), `adapter`, `total`, `out` | Đã mở GPU, sắp render |
| `output_reserved` | `path` | Đã tạo file tạm này, cạnh file đích |
| `encoder` | `name` | Encoder đã mở |
| `progress` | `frame`, `total`, `elapsed_ms` | Xong `frame` khung trên `total`, sau mỗi khung |
| `finished` | `report`: `encoder`, `frames`, `audio_samples`, `clamped_samples`, `pcm_sha256` | File đã nằm ở đích |
| `cancelled` | | Đã dừng và đã xoá file tạm |
| `error` | `message` | Thất bại, kèm lý do bằng tiếng Anh |

- Mỗi lượt chạy kết thúc bằng đúng một trong `finished`, `cancelled`, `error`. Lỗi đọc tham số và đọc project cũng thành `error`. Mã thoát là 0 chỉ khi `finished`.
- `finished` và `error` được ghi ngay khi kết quả đã rõ (sau bước đổi tên, hoặc ngay khi một bước trước đó thất bại), trước khi giải phóng GPU. Tiến trình crash hoặc treo sau sự kiện kết thúc không làm đổi kết quả: bên gọi tin sự kiện kết thúc, kể cả khi mã thoát khác 0, và tự kill tiến trình nếu nó không thoát sau một khoảng chờ.
- Bên đọc phải bỏ qua dòng không phải JSON hợp lệ và `event` chưa biết, vì thư viện có thể ghi vào stdout và sự kiện có thể được bổ sung về sau. `protocol` chỉ tăng khi một sự kiện có sẵn mất hoặc đổi trường.
- Gửi dòng `cancel` vào stdin để dừng sạch. Mặc định stdin đóng không có ý nghĩa gì. Thêm cờ `--cancel-on-stdin-eof` (chỉ dùng được với `--progress json`) thì stdin đóng cũng là huỷ: server headless phải bật cờ này để tiến trình con tự dừng khi server chết, vì khi đó stdin đóng theo. Editor không dùng cờ, vì đã có job object kill-on-close.
- Một dòng không dài quá 64 KB; thông báo `error` quá dài bị cắt bớt ngay trong JSON. Nếu đã xong mọi khung mà dòng cuối không đọc được thì đó có thể là sự kiện kết thúc: bên gọi không được xoá file tạm và phải báo là không rõ kết quả.
- Tiến trình chết mà không có sự kiện kết thúc (crash, bị kill) thì bên gọi tự xoá các file đã báo qua `output_reserved`. Đã có sự kiện kết thúc thì không được xoá: `error` có thể đang giữ lại file đã render xong khi đổi tên thất bại.
- stderr chỉ để con người đọc. Bên gọi phải đọc liên tục để pipe không đầy, và chỉ giữ một đoạn cuối có giới hạn.

## Lỗi

Kết quả lỗi có `code`, `message` và dữ liệu kèm theo. `EditError` đổi sang code ổn định, ví dụ `overlap`, `clip_not_found`, `no_room`, `invalid_project`. Thêm `stale_revision`, `busy` (đang kéo hoặc đang xuất), `no_app` (chế độ live không tìm thấy app), `gpu_unavailable`, `invalid_params`, `out_of_range`, `unsaved_changes` (`project_new`, `project_open` trên project chưa lưu mà thiếu `discard_changes`; ở chế độ live luôn trả lỗi này, vì agent không được bỏ việc chưa lưu của user), `project_locked`, `source_not_found`, `invalid_source`, `package_not_found`, `file_exists`, `job_not_found`, `capture_failed`, `not_text` (`text_set` trên clip không phải chữ). Lỗi không làm chết server. GPU hỏng thì báo lỗi rõ và phải khởi động lại phiên.

## An toàn

- Pipe chỉ cho user hiện tại truy cập, không mở cổng mạng.
- App chỉ nhận lệnh khi user bật tuỳ chọn, và hiện dấu báo đang có agent nối vào.
- Đường dẫn trong tham số được chuẩn hoá. Lưu, xuất và ghi đè không bao giờ ghi vào file nguồn đang dùng, theo kiểm tra sẵn có của `ExportJob`. `project_save` còn từ chối file đích của lượt xuất đang chạy, và trả `file_exists` với file khác đã có nếu thiếu `overwrite: true`.
- Lúc app đang mở hộp thoại, prompt hoặc đang tải, lệnh sửa, undo, redo và `export_start` trả `busy`.
- Package là code shader nạp từ đĩa. Agent chỉ thêm và mở được package từ package root (`ProjectPaths::roots_only`). Ở chế độ live, project do chính user mở bằng tay vẫn dùng được package cạnh project như trước.

## Cách chạy

`gpufx-mcp.exe` nằm cạnh `gpufx-render.exe`. Mặc định làm việc trên project file (`--project <file>` để mở sẵn). `--live` nối vào cửa sổ đang mở; user bật tại File > cho phép agent điều khiển, tên pipe gắn với SID của user. Server khi chế độ file: tiến trình xuất không nằm trong job object kill-on-close, để stdin đóng thì nó tự dừng sạch.

## Thứ tự làm

Mỗi bước một commit, build và test xong mới sang bước sau.

1. Crate `gpufx-mcp`, trait `EditorSession`, `FileSession`, các tool đọc và sửa, undo.
2. `frame_capture` ở chế độ file, mã hoá PNG.
3. `export_*` ở chế độ file.
4. Cổng lệnh trong `gpufx-editor`, `LiveSession`, kiểm tra revision và trạng thái bận.
5. `frame_capture` và `export_*` ở chế độ live.

## Kiểm thử và tiêu chí xong

| Tiêu chí | Cách kiểm |
|---|---|
| Tool sửa đúng | Mỗi tool cho cùng project với gọi thẳng lệnh trong `engine` |
| Revision chống sửa nhầm | Gọi với revision cũ bị từ chối và project không đổi |
| Hai chế độ giống nhau | Cùng chuỗi lệnh cho project bằng nhau trên `FileSession` và `LiveSession` |
| Chụp đúng khung | Hash khung của `frame_capture` trùng hash CLI tại cùng index; `time` và `frame` tương ứng cho cùng ảnh |
| Chụp live không làm phiền | Đầu phát và khung đang hiển thị không đổi |
| Xuất qua MCP | Hash trùng CLI, huỷ không để lại file tạm |
| Dùng thật | Một client MCP thật dựng video ngắn trên cả hai chế độ, có người xem kết quả |

## Phụ thuộc và rủi ro

- `ClipKey` theo chỉ số là điểm yếu cho agent. Nếu `expected_revision` gây thử lại quá nhiều, cần chuyển sang id clip ổn định trong `engine`.
- Chế độ file và app đang mở cùng sửa một project file sẽ ghi đè lẫn nhau. File khoá `<project>.lock` giữ không chia sẻ và tự xoá khi đóng, kể cả khi tiến trình chết; lần mở thứ hai bị từ chối (`project_locked`, hoặc thông báo trong app).
- Chế độ file mở GPU riêng, nên chạy cùng app tốn thêm bộ nhớ video.
- Độ dài clip đọc từ file nguồn bằng `d3d12_backend::probe_media`, tách theo luồng video và audio; giao diện đã dùng nó thay cho độ dài cố định 10 giây. Agent dùng lại API này.
