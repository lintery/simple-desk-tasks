# Звуки фокуса из записей

Дождь, природа, вентилятор и поезд в виджете «Фокус» играют полевые записи с
[Freesound](https://freesound.org), все под CC0. Здесь лежит то, из чего сделаны петли
в релизе [`sounds-v1`](https://github.com/lintery/simple-desk-tasks/releases/tag/sounds-v1).
Кто что записал — в [CREDITS.md](CREDITS.md).

```powershell
python -I tools/sounds/fetch.py <src>          # записи из sources.json -> <src>/<id>.ogg
python -I tools/sounds/loops.py <src> <out>    # петли <out>/<id>.opus и manifest.json
python -I tools/sounds/check.py <out>          # шов и громкость каждой петли
python -I tools/sounds/cors_server.py 8771 <out>   # послушать: http://127.0.0.1:8771/listen.html
```

Нужен ffmpeg с libopus. `loops.py` берёт из записи самый ровный кусок до 3 минут (у грозы —
самый насыщенный), сводит конец с началом за 4 секунды и выводит громкость на −24 LUFS одним
постоянным усилением с быстрым ограничителем на пиках (уровень на обоих концах петли одинаковый).
`loops.py <src> <out> rain fan` пересобирает только перечисленные звуки.

Новые петли — это новый релиз (`sounds-v2`): выложить файлы, затем вписать адрес и
SHA-256 каждого файла в `src-tauri/src/sounds.rs`. Приложение сверяет каждую скачанную петлю
с этой суммой.
