# Kế hoạch mở rộng sticker, effect, transition và filter FFmpeg

Ngày lập: 2026-11-18. Trạng thái: đã duyệt 2026-11-18, đang thực hiện.

## Mục tiêu

- Tham khảo các editor open-source để bổ sung sticker, effect, transition, animation vào editor.
- Port thêm filter FFmpeg sang gói `gpufx` trong `external/FFmpegGpuFilter/packages/gpufx/ffmpeg`.
- Repo editor và lõi gpufx giữ MIT. Được clone nguyên văn mã/ảnh GPL-3 (OpenShot, Kdenlive, Shotcut, frei0r bản GPL-2.0+ dùng theo GPL-3), với điều kiện (user chốt 2026-11-18):
  - Mỗi gói GPL nằm trong thư mục đóng gói riêng, kèm file `LICENSE` GPL-3 và `README.md` dẫn nguồn (repo, đường dẫn file gốc, tác giả, commit).
  - Mã GPL chỉ ở trong gói GPL, không chép vào gói MIT, `gpufx-core` hay editor; gói MIT không dùng chung file với gói GPL.
  - Nguồn GPL-2.0-only (không có "or later") không trộn vào gói GPL-3.
  - Nguồn MIT/BSD/Apache/CC0/CC BY thì giữ giấy phép gốc của nguồn đó.

## Hiện trạng

| Nhóm | Đang có | Ở đâu |
|---|---|---|
| Filter FFmpeg đã port | 108 gói | `external/FFmpegGpuFilter/packages/gpufx/ffmpeg` |
| Transition | xfade (đủ bộ của FFmpeg), wipe/slide/zoom/pixelize/circle-reveal/dissolve; TqkLibrary.FFmpegTransition (clock-wipe, collapse, crossfade, dip-to-color, expand); TanersenerSlideShow (blinds, box-in, checkerboard, push-box, push-film, sliding-bars…); avs-transition 8 gói | submodule gói và `external/GpufxVideoEditor-Assets` |
| Effect/look | avs-effect 6, avs-look 8, avs-mask 7, tqk2811-effect 1, standalone 4 (falling-ginkgo, falling-maple, gradient-map, rgb-split) | như trên |
| Animation clip | `avs`, `tqk2811` (đường cong dx/dy/scale + gói effect) | `GpufxVideoEditor-Assets/animations` |
| Kiểu chữ | 4 bộ avs | `GpufxVideoEditor-Assets/text-styles` |
| Sticker, âm thanh | gói zip trong release của assets | release `GpufxVideoEditor-Assets` |
| CapCut | khoảng 154 gói, repo riêng | `FFmpegGpuFilter-packages-capcut` |

Giới hạn quan trọng: effect **không giữ trạng thái giữa các frame**. Filter cần frame trước/sau (khử nhiễu theo thời gian, deinterlace, tmix, deflicker, chống rung) chưa làm được nếu engine không cấp lịch sử frame.

## Các editor open-source tham khảo

| Project | Giấy phép | Lấy được gì | Cách lấy |
|---|---|---|---|
| gl-transitions | MIT (từng file ghi tác giả, đa số MIT) | ~80 transition GLSL: cube, page curl, ripple, swirl, doorway, morph, burn, crosswarp, dreamy, kaleidoscope, glitch… | Port shader sang WGSL/HLSL của gpufx, giữ ghi công từng file. Lọc file có giấy phép khác MIT |
| OpenShot | GPL-3 | Bộ ~400 ảnh luma wipe (transition theo ảnh xám) | Clone bộ ảnh luma vào gói GPL-3 `luma-wipe` (shader nhận ảnh xám), LICENSE GPL-3 + README dẫn nguồn |
| Kdenlive / Shotcut (MLT, frei0r) | GPL | Danh mục effect: glow, old film, VHS/glitch, light leak, cartoon, posterize, halftone, mosaic, TV CRT, letterbox, spot remover, shape mask, text typewriter | Port nguyên shader/thuật toán vào các gói GPL-3 riêng (frei0r chỉ lấy file GPL-2.0+) |
| Olive | GPL-3 | Node màu OCIO, LUT | Ý tưởng: hỗ trợ `.cube` LUT |
| Blender VSE | GPL | Glow, speed, wipe, gaussian blur, color balance (lift/gamma/gain) | Ý tưởng |
| OpenCut | MIT | Danh mục sticker/text preset kiểu CapCut | Tham khảo UI |
| Natron / Lightworks | GPL / đóng | Film grain, lens flare | Ý tưởng |

Nguồn sticker có giấy phép dùng được:

| Bộ | Giấy phép | Ghi chú |
|---|---|---|
| Noto Emoji (kể cả Animated Noto Emoji) | Apache-2.0 / OFL | Có bản động Lottie/WebP, hợp làm sticker động |
| Fluent Emoji (Microsoft) | MIT | Ảnh 3D, flat, high contrast |
| Twemoji | CC BY 4.0 | Cần ghi công |
| OpenMoji | CC BY-SA 4.0 | Share-alike, để gói riêng có LICENSE riêng |
| Lottie trên LottieFiles mục "free" | Lottie Simple License | Phải kiểm từng file, rủi ro cao, **đề xuất bỏ** |

Sticker động: editor hiện nạp ảnh tĩnh/video? Nếu chưa nạp được WebP/GIF động hay Lottie thì cần thêm bộ giải mã (xem giai đoạn S2).

## Danh sách đề xuất

### Transition mới (ưu tiên từ gl-transitions, MIT)

Đợt T1, 20 cái phổ biến nhất: `cube`, `pagecurl` (InvertedPageCurl), `ripple`, `swirl`, `doorway`, `burn`, `crosswarp`, `crosszoom`, `dreamy`, `kaleidoscope`, `glitch-displace`, `morph`, `mosaic`, `polar-function`, `radial`, `squeeze`, `window-slice`, `heart`, `bounce`, `film-burn`.

Đợt T2: phần còn lại của gl-transitions trừ các cái trùng xfade, cộng gói GPL-3 `luma-wipe` với bộ ảnh luma clone từ OpenShot.

### Effect/look mới (tự viết, lấy danh mục từ Kdenlive/Shotcut/CapCut)

| Nhóm | Mục |
|---|---|
| Phong cách | glow/bloom, old film (sọc, rung sáng, hạt), VHS, CRT scanline, halftone, posterize, cartoon/toon, oil paint (Kuwahara), sketch, emboss |
| Ánh sáng | light leak, lens flare (tham số tâm), god rays, vignette màu |
| Chuyển động trong clip | shake (đã có), zoom pulse theo nhịp (dùng dò nhịp đã có), RGB glitch nhấp nháy, strobe |
| Khung | letterbox/cinema bars, blur nền dọc→ngang (fill blur), khung bo góc + bóng |
| Màu | LUT `.cube` (gói nhận file LUT), lift/gamma/gain, split toning, auto white balance (grayworld) |
| Hạt rơi | tuyết, mưa, tim, bong bóng, confetti (giống falling-maple) |

### Animation clip và kiểu chữ

- Thêm 15–20 animation in/out/combo (bounce, elastic, flip 3D, rotate-in, swing, typewriter cho chữ, wave theo ký tự) vào `GpufxVideoEditor-Assets/animations/tqk2811`.
- Thêm 10 kiểu chữ (neon, outline đôi, gradient, viền sticker, karaoke tô dần).

### Sticker

- S1: gói tĩnh từ Fluent Emoji (MIT) + Noto Emoji (Apache), chia nhóm cảm xúc, mũi tên, khung, nhãn "Subscribe/Like" tự vẽ SVG.
- S2: sticker động (Animated Noto Emoji). Cần quyết định định dạng (xem câu hỏi Q3).

## Filter FFmpeg còn thiếu để port

Chia theo độ khó. Chỉ tính filter video, không cần lịch sử frame trước.

| Đợt | Filter | Ghi chú |
|---|---|---|
| F1 – dễ, giá trị cao | `eq`, `hue`, `lut3d`, `haldclut`, `colorspace` (chỉ matrix/primaries), `negate` (nếu `invert` chưa đủ), `rotate`, `hflip`/`vflip`/`transpose`, `crop`, `pad`, `avgblur`, `gblur` (nếu khác gaussian-blur), `dblur`, `boxblur` đã có | `lut3d` cho phép nạp `.cube`, cần cho mục LUT ở trên |
| F2 – hình thái và cạnh | `erosion`, `dilation`, `inflate`, `deflate`, `morpho`, `sobel`, `prewitt`, `roberts`, `scharr`, `guided`, `bilateral` đã có, `chromaber` (giống chromaber_vulkan), `elbg` (posterize) | Shader 3x3, nhanh |
| F3 – màu tự động | `grayworld`, `greyedge`, `colorconstancy`, `normalize` (cần thống kê cả frame, làm bằng reduce 2 pass), `histeq` | Cần pass reduce trên GPU |
| F4 – khử nhiễu một frame | `nlmeans`, `vaguedenoiser`, `fftdnoiz` (chế độ một frame), `bm3d` (nặng, làm sau) | Nặng, cần đo tốc độ |
| F5 – phóng ảnh pixel art và tiện ích | `xbr`, `hqx`, `super2xsai`, `epx`, `shufflepixels`, `tile`, `remap`, `datascope`, `oscilloscope`, `zoneplate`, `cellauto`, `life`, `sierpinski` | Nguồn và kiểm tra |
| F6 – cần lịch sử frame | `tmix`, `tblend`, `lagfun`, `amplify`, `deflicker`, `atadenoise`, `hqdn3d` (phần thời gian), `tmedian`, `yadif`/`bwdif`/`w3fdif`, `minterpolate`, `deshake`/`vidstab` | Phải thêm vào gpufx-core API cấp N frame trước cho node. Đề xuất làm sau cùng, có spec riêng |

Trước khi port, chạy `ffmpeg -filters` trên bản trong `third_party/ffmpeg` để đối chiếu đúng danh sách còn thiếu (tên gói hiện tại không trùng tên filter, vd `gaussian-blur`, `color-adjust`).

Mỗi gói port phải có test so pixel với FFmpeg CPU như các gói hiện có (chung cách đo sai số).

## Lộ trình đề xuất

Làm theo nhánh riêng ở từng repo, mỗi đợt một nhánh, mỗi gói một commit.

1. **P0 – khảo sát chính xác** (phiên chính, nhỏ): đối chiếu `ffmpeg -filters`, kiểm danh sách gl-transitions và giấy phép từng file, kiểm editor có đọc được WebP động/GIF không.
2. **P1 – F1 + F2** trong submodule FFmpegGpuFilter (báo phiên `ffmpeggpufilter` trước). Giao subagent theo lô 5–8 filter.
3. **P2 – T1** 20 transition gl-transitions vào gói mới `packages/gpufx/gl-transitions` (MIT, có NOTICE ghi tác giả).
4. **P3 – Effect/look** nhóm Phong cách + Ánh sáng + Hạt rơi vào `GpufxVideoEditor-Assets/packages/gpufx/tqk2811-effect`.
5. **P4 – Sticker S1** + animation + kiểu chữ trong assets, cập nhật script build zip.
6. **P5 – F3, F4, F5, T2, luma-wipe.**
7. **P6 – lịch sử frame trong gpufx-core** (spec riêng) rồi F6, sticker động S2.

Mỗi bước: build, chạy test (tối đa 8 luồng, 4 nếu có MapleStory.exe), kiểm thư mục `packages` không rỗng để tránh test tự skip, chụp ảnh preview vài mẫu.

## Rủi ro

- Giấy phép: gl-transitions có vài file không phải MIT; OpenMoji share-alike; Lottie free không rõ. Mỗi gói kèm LICENSE/README dẫn nguồn riêng. Gói GPL-3 đặt ở thư mục riêng (`packages/gpufx/gpl3/<nguồn>/`), package manager nên hiện giấy phép từng gói.
- Không chép mã, ảnh, preset tên thương mại CapCut vào repo MIT (đã có repo capcut riêng).
- Khối lượng lớn: P1–P4 ước khoảng 100+ gói, cần lô agent và chi phí token đáng kể.
- Filter thống kê cả frame (F3) và khử nhiễu (F4) có thể không đạt realtime trên preview.

## Câu hỏi cần user chốt

- Q1 (chốt 2026-11-18 theo đề xuất): Thứ tự ưu tiên: filter FFmpeg trước (P1) hay transition/effect cho người dùng thấy ngay (P2–P3) trước? Đề xuất: P2 + P1 song song.
- Q2 (chốt 2026-11-18 theo đề xuất): Gói gl-transitions để trong submodule FFmpegGpuFilter (`packages/gpufx`) hay trong `GpufxVideoEditor-Assets`? Đề xuất: FFmpegGpuFilter, cạnh xfade.
- Q3 (chốt 2026-11-18 theo đề xuất): Sticker động dùng định dạng gì: WebP/GIF động (giải mã qua FFmpeg sẵn có) hay Lottie (cần thư viện vẽ vector)? Đề xuất: WebP động.
- Q6 (đã chốt 2026-11-18): gói effect/transition gpufx bản GPL-3 đặt ở `external/FFmpegGpuFilter/packages/gpufx/gpl3/<nguồn>/`. Nội dung GPL-3 chỉ dành cho editor (sticker, animation, kiểu chữ) đặt trong `GpufxVideoEditor-Assets`, cũng thư mục riêng có LICENSE + README nguồn.
- Q4 (chốt 2026-11-18 theo đề xuất): Có nhận OpenMoji (CC BY-SA) không, hay chỉ MIT/Apache/CC BY? Đề xuất: chỉ MIT/Apache/CC BY.
- Q5 (chốt 2026-11-18 theo đề xuất): Có làm F6 (lịch sử frame trong gpufx-core) trong đợt này không? Đề xuất: để đợt sau.
