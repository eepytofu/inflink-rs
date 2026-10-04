---
"frontend": minor
---

feat: 从网易云缓存的音频文件头读取采样率和位深，Discord 状态的音质行可以显示 `FLAC 24-bit/48 kHz`；之前播放过的歌曲（音频流信息从磁盘缓存恢复）不再缺少采样率；`getCurrentAudioInfo()` 的 `bitDepth` 现在对 FLAC 有值
