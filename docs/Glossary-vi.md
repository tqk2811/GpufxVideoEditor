# Thuật ngữ

## B-frame
Khung video được mã hoá dựa vào cả khung trước và khung sau, nên thứ tự giải mã khác thứ tự hiển thị.

## D3D12VA
API giải mã video bằng phần cứng trên Direct3D 12, được FFmpeg hỗ trợ.

## Fence
Đối tượng đồng bộ của GPU: một bên báo đã làm tới giá trị nào, bên kia chờ tới giá trị đó.

## GOP
Nhóm khung bắt đầu từ một keyframe. GOP càng dài thì tua càng phải giải mã nhiều khung hơn.

## Zero-copy
Xử lý trực tiếp trên texture có sẵn, không sao chép dữ liệu sang vùng nhớ khác.
