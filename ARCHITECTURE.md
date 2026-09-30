# Kadr — Native AI Video Editor · Architecture

Рабочее название продукта: **Kadr** (от «кадр»). Формат проекта: `*.kadr` (JSON).

## 1. Выбранный stack (результат исследования, сентябрь 2026)

| Область | Выбор | Почему | Альтернативы |
|---|---|---|---|
| Язык | Rust 1.98 (stable) | требование ТЗ | — |
| GUI | **Slint 1.18** (winit + femtovg/OpenGL, fallback software renderer) | нативный, декларативный `.slint`, HiDPI, `ContextMenuArea`, `FocusScope`, `SharedPixelBuffer` для кадров из фоновых потоков, GPL/Royalty-free/commercial лицензии | egui: immediate-mode, сложно получить «коммерческий» вид и сложные layouts; iced: хорош, но нет готовых контекстных меню/popup-инфраструктуры и медленнее эволюция виджетов. Объективных препятствий для Slint не найдено. |
| Media | **FFmpeg 8 CLI** (`ffmpeg`/`ffprobe`) за трейтом `MediaBackend` | работает уже сейчас без dev-библиотек и линковки, изоляция процесса (краш декодера ≠ краш редактора), GPL-сборка не линкуется в бинарь | `ffmpeg-next 9` (libav bindings) — план для V0.2+ для низколатентного seek/decode в preview; GStreamer — избыточен для NLE-ядра |
| Audio out | **cpal 0.18** (WASAPI) | стандарт де-факто, low-level, без лишних слоёв | rodio (надстройка, но микширование нам нужно своё) |
| GPU | **wgpu 30** — *отложено* | композицию делает `CpuRenderer` (многопоточный, байт-в-байт детерминированный); GPU-рендерер встанет за тем же контрактом `kadr-render` | — |
| HTTP | reqwest 0.13 + `native-tls` (SChannel) | без cmake/nasm (`aws-lc-rs`), системные сертификаты Windows | rustls + ring |
| Credentials | keyring 4 → Windows Credential Manager | ключи не хранятся в проекте/конфиге | DPAPI вручную |
| Async | Tokio **только** в AI-слое (сетевые запросы) | медиа-работа — CPU/процессы, ей достаточно потоков | — |
| Логи | tracing + tracing-appender (JSON-файл + консоль) | structured logging | — |
| Toolchain | `x86_64-pc-windows-gnu` (dev) | не требует Visual Studio; переход на MSVC — `rustup default stable-msvc` | MSVC для release |

## 2. Диаграмма

```
                         ┌──────────────────────────── apps/editor ────────────────────────────┐
                         │  Slint UI (.slint)  ⇄  AppController (UI thread)                     │
                         │     │ callbacks            │ models (virtualized)                    │
                         │     ▼                      ▼                                         │
                         │  PreviewPlayer ──frames──► Image      AudioPlayer (cpal) ◄─ PCM cache │
                         └─────┬──────────────┬──────────────┬───────────────┬─────────────────┘
                               │              │              │               │
             ┌─────────────────▼──┐  ┌────────▼────────┐ ┌───▼──────┐  ┌─────▼──────────┐
             │ kadr-timeline      │  │ kadr-jobs       │ │ kadr-ai  │  │ kadr-media     │
             │ EditEngine, Command│  │ queue/priority/ │ │ EditCmd  │  │ MediaBackend   │
             │ Undo/Redo, snapping│  │ progress/cancel │ │ validate │  │ └ FfmpegCli    │
             │ scene evaluator    │  │ retry           │ │ intents  │  │ probe/thumbs/  │
             └────────┬───────────┘  └─────────────────┘ │ providers│  │ source decode/ │
                      │                                  │ cost/    │  │ pcm/ encoder   │
             ┌────────▼───────────┐  ┌─────────────────┐ │ privacy  │  └────────────────┘
             │ kadr-project       │  │ kadr-analysis   │ └──────────┘
             │ model + .kadr I/O  │  │ peaks, silence  │  ┌─────────────────┐
             │ atomic save/autosave│ └─────────────────┘  │ kadr-cache      │
             └────────┬───────────┘                       │ keys, versioning│
             ┌────────▼───────────┐                       └─────────────────┘
             │ kadr-core          │  время (flicks), FrameRate, timecode, ids
             └────────────────────┘
```

Правила зависимостей:
- `core` ни от чего не зависит; `project` → `core`; `timeline` → `project`.
- `ai` → `timeline`/`project` (исполняет команды через EditEngine), **не** зависит от Slint.
- `timeline` не знает об AI-провайдерах и рендерерах: он только вычисляет `FrameScene` (`timeline::scene::evaluate`). `media` не знает о проекте (работает с путями/таймкодами): декодирует, масштабирует и кодирует кадры, композицию не делает.
- `scene` (контракт) ← `render`, `playback`, `timeline`; рендерер не видит таймлайн, таймлайн не видит рендерер; `playback` с `render` знают друг друга только через `FrameScene`.
- Только `apps/editor` зависит от Slint.

## 3. Crate boundaries

| Crate | Ответственность |
|---|---|
| `kadr-core` | `Time` (i64 flicks), `FrameRate` (рациональный), конвертации кадр↔время с округлением, timecode (включая drop-frame отображение), `Id`-типы |
| `kadr-project` | Project, MediaAsset, Bin, Sequence, Track, Clip, Transition, Effect, Keyframe, Marker, Transcript, AnalysisResult, AIAction, EditorPreferenceEvent, MulticamGroup (ракурсы + смещения синхронизации), StoredDecision (решения Jev с вердиктом человека); сериализация `.kadr` c `format_version`; атомарная запись; autosave/recovery |
| `kadr-timeline` | EditEngine: команды (Split/Trim/Move/Delete/Insert/ChangeProperty/AddTransition/SwitchAngle/SetAngleRange/Batch), мультикам-клипы (время группы ↔ таймлайн ↔ источник ракурса), undo/redo, ripple, snapping, linked A/V; вычислитель сцены `scene::evaluate` (что видно в момент t → `FrameScene`) и `composition::audio_segments` (что слышно) |
| `kadr-media` | `MediaBackend` trait; `FfmpegCli` backend: probe, thumbnails, decode кадра, декодирование исходников (`open_source`, `decode_still`), масштабирующий поток для анализа, извлечение PCM, `FrameEncoder` (RGBA-кадры + аудио-граф → libx264/aac mp4 с прогрессом). FFmpeg только демультиплексирует, декодирует, масштабирует и кодирует — не компонует |
| `kadr-scene` | контракт рендера: `FrameScene` (слои снизу вверх, размещения в пикселях холста, ссылки на media, а не пиксели), `OutputSpec`, `RenderQuality`, операции переходов |
| `kadr-render` | `CpuRenderer`: `FrameScene` + готовые кадры источников → RGBA буфер (цвет, crop, поворот, прозрачность, blend, transitions), фиксированная математика — одинаковый результат при любом числе потоков |
| `kadr-playback` | разрешение сцены в кадры: `Resolver` (декодеры, кэш кадров, пул буферов), `PreviewPlayer` (живой preview, часы аудио), `export` (кадр за кадром → энкодер, A/V sync) |
| `kadr-project-scenes` | `ProjectScenes`: снимок проекта как `SceneSource` для playback (оценка сцены в момент `t`, пути media, offline-детектор) |
| `kadr-audio` | воспроизведение: cpal-поток + микшер клипов из PCM-кэша, мастер-часы воспроизведения |
| `kadr-analysis` | детерминированный анализ: пики waveform, RMS/loudness, детекция тишины; изображение — резкость/экспозиция/тряска/чёрный кадр по уменьшенным кадрам, границы планов (`VideoOverview`, `ShotSummary`); синхронизация ракурсов по огибающей звука (корреляция Пирсона, мин. перекрытие — половина короткого клипа) или по таймкоду |
| `kadr-jobs` | фоновые задачи: priority queue, worker pool, progress, cancellation token, retry с backoff, error state |
| `kadr-cache` | каталог кэша, ключ = blake3(path+size+mtime) + версия алгоритма; инвалидация |
| `kadr-ai` | Edit Command Language + валидация; локальный intent-парсер; `AIProvider` + REST реализации; тарифы/стоимость; бюджеты; privacy-политика; хранение ключей; модуль `jev`: `JevDecisionService`, шаблоны вопросов, гейты, персонализация (§11) |
| `kadr-i18n` | локализация RU/EN: JSON-каталоги (`locales/*.json`, встроены в бинарник), плюрализация, локализованные длительности; общий источник строк для Slint и Rust |
| `apps/editor` | Slint UI, контроллер, preview/audio плееры, связывание всего |
| `kadr-mcp-bridge` | HTTP/1.1 сервер на `127.0.0.1:0` внутри процесса редактора: JSON-RPC диспетчер Kadr-инструментов (`get_state`, `import_media`, `edit`, …) на UI-потоке через `post(...)`; bearer-токен, файл обнаружения `mcp.json` |
| `apps/kadr-mcp` | Отдельный бинарник — stdio MCP-сервер (JSON-RPC 2.0, протокол `2025-06-18`) для Claude: агрегирует `ui_*` (проксирует в embedded MCP-сервер Slint 1.18) и Kadr-инструменты (проксирует в `kadr-mcp-bridge`); обнаруживает/запускает headless-инстанс Kadr |

Сознательно **не** создаём отдельные `transcription`, `platform`: пока у них нет содержимого. Jev живёт модулем `kadr_ai::jev` — ему нужны те же CostGuard, PrivacyPolicy и ledger, что и чату.

## 4. Модель времени

- Внутренняя единица: **flick** = 1/705 600 000 с (`Time(i64)`). Делится нацело на длительность кадра при 23.976, 24, 25, 29.97, 30, 48, 50, 59.94, 60, 120 fps и на период сэмпла 8–192 kHz. Диапазон i64 ≈ 414 лет.
- `FrameRate { num, den }` — рациональный (24000/1001 и т.д.). `time_to_frame` использует floor, `frame_to_time` — точное целое.
- Секунды в `f64` существуют только на границах: UI (пиксели), FFmpeg-аргументы (печать как `sec.micros`), JSON EditCommand (`*_ms`).
- VFR-медиа: клип адресует источник временем, а не номером кадра; последовательность имеет фиксированный fps, snapping к кадрам последовательности.

## 5. Project model

```
Project { format_version, id, name, settings, bins[], assets[], sequences[], active_sequence,
          transcripts[], analysis[], ai_log[], preference_events[], cost_ledger }
MediaAsset { id, path (абсолютный + относительный к проекту), kind, probe: MediaInfo, bin }
Sequence { id, name, frame_rate, width, height, sample_rate, tracks[], markers[], in_out }
Track { id, kind: Video|Audio, name, muted, solo, locked, height, clips[] (отсортированы) }
Clip  { id, asset, source_in, source_out, timeline_in, (timeline_out = in + dur/speed),
        link_group, speed, transform, color, audio: {gain_db, pan, fade_in, fade_out},
        effects[], keyframes }
Transition { id, kind, duration, from_clip, to_clip }
```
Исходные media никогда не модифицируются; clip только ссылается на `asset` и диапазон источника.

## 6. Edit engine / Undo

Command Pattern с **сохранением обратной операции**: каждая команда `apply(&mut Sequence) -> Result<Undo>`.
Для надёжности используется гибрид: элементарные команды хранят минимальный «снимок затронутых треков» (`TrackPatch { track_id, before: Vec<Clip>, after: Vec<Clip> }`). Это O(размер трека), а не O(проект), и делает любую команду (в т.ч. ripple по всем трекам) тривиально обратимой без ручного написания inverse для каждой. `Batch` (в т.ч. `AIEditBatch`) = одна запись в истории → «Undo AI edit» одним действием.
История пишется в `Project.history_log` (тип, время, источник: user/ai/voice).

## 7. Media pipeline

```
Import → probe (ffprobe JSON) → MediaAsset
       → jobs: thumbnails (N кадров, 160px, JPEG→RGBA в кэше)
               audio proxy (ffmpeg → s16le 48 kHz stereo .pcm в кэше)
               → waveform peaks (из PCM) → silence analysis
               (V0.2: video proxy H.264 540p all-intra для быстрого seek)
Preview:  seek/scrub/play → PreviewPlayer (kadr-playback): FrameScene в момент t → Resolver (декодированные кадры источников) → CpuRenderer → буфер кольца → Slint Image; синхронизация по аудио-часам
Export:   те же FrameScene по кадрам → Resolver → CpuRenderer → FrameEncoder (kadr-media: RGBA-кадры + аудио-граф atrim/adelay/volume/amix → libx264/aac mp4), прогресс и отмена через kadr-jobs
```

## 8. Rendering pipeline

Одна цепочка для preview, экспорта и MCP `get_frame`:

```
Timeline ──evaluate(t, OutputSpec)──► FrameScene ──► kadr-playback Resolver ──► CpuRenderer ──► preview / FrameEncoder
 (kadr-timeline::scene)              (kadr-scene)     (кадры источников:          (kadr-render)
                                                       FFmpeg-декодеры, кэш)
```

1. **Scene evaluation** (`kadr-timeline::scene`) — чистая функция: проект, последовательность, время → `FrameScene`: видеодорожки снизу вверх, каждая — слой (клип или переход) с размещением в пикселях холста, crop, прозрачностью, blend и цветокоррекцией; невидимое отбрасывается (`cull`), чтобы не декодировалось. `has_video_at` — тот же вопрос «есть ли видео под playhead» для UI. Аудио-часть — `timeline::composition::audio_segments`.
2. **Resolve** (`kadr-playback`) — `Resolver` превращает ссылки на media в декодированные кадры (FFmpeg `open_source`, стабильная нормализация частоты и цвета, кэш, пул буферов, read-ahead); `ProjectScenes` (`kadr-project-scenes`) — снимок проекта для него.
3. **Render** (`kadr-render`) — `CpuRenderer` компонует слои в RGBA; качество `PreviewFast`/`PreviewHigh`/`Export` выбирает `OutputSpec`. Preview и экспорт дают один и тот же кадр при одном `OutputSpec`.
4. **Output** — preview: кадр сразу в буфер дисплея (`apps/editor::preview_cpu`); экспорт: `kadr_playback::export` подаёт кадры в `FrameEncoder` (`kadr-media`), независимо от preview, параллельно монтажу.

## 9. Threading model

- **UI thread** (Slint event loop): только модель UI и лёгкие вычисления; тяжёлое — никогда.
- **Job workers** (`kadr-jobs`, N = cores-1, priority queue): probe, thumbnails, PCM, анализ, export. Результаты → UI через `slint::invoke_from_event_loop`.
- **Preview player** (`kadr-playback`): свой поток рендера кадров (latest-wins при scrub, по аудио-часам при воспроизведении), декодеры — процессы FFmpeg с read-ahead; рендер — пул потоков `CpuRenderer`.
- **Audio**: cpal callback (real-time, без аллокаций и блокировок — lock-free ring buffer) + feeder-поток, читающий PCM-кэш и микширующий треки.
- **Tokio runtime** (1–2 потока) — только AI HTTP.
- **Autosave** — таймер UI сериализует проект в память, запись на диск в фоне.

## 10. Timeline UI

Не тысячи widgets: контроллер вычисляет **только видимые** клипы для текущих scroll/zoom (виртуализация) и отдаёт плоскую модель прямоугольников `ClipView {x, w, track_row, ...}`; waveform-полосы рендерятся в Rust в `SharedPixelBuffer` на видимую ширину. Hit-testing, drag, trim, snapping считаются в Rust в координатах времени.

## 11. AI architecture

```
Natural language (chat / voice→STT)
   → IntentRouter
       ├─ LocalIntentParser (regex/грамматика, $0): «удали паузы длиннее N с», «разрежь здесь», «undo»…
       └─ LLM (AIProvider, только если разрешено политикой и бюджетом) → JSON EditCommand[]
   → Plan (что найдено, сколько операций, стоимость, confidence) → [Apply | Review | Cancel]
   → Validator (schema, диапазоны, permission, project-state) → EditEngine::apply(Batch) → Undo AI edit
   → ai_log (AIAction) + EditorPreferenceEvent при человеческих правках
```
- `AIProvider` trait: `complete(Request) -> Response{usage}`; реализации: OpenAI-совместимый (OpenAI, custom, local llama.cpp/Ollama), Anthropic, Jev (V0.2).
- Уровни: `Local | Economy | Smart | Director`, каждому соответствует (provider, model) из настроек.
- **CostGuard**: оценка токенов до запроса → проверка лимитов (request/session/project/month) → при превышении требуется явное подтверждение; фактический usage → ledger (сессия, проект, месяц).
- **PrivacyPolicy**: `Off | LocalOnly | AskBeforeCloud | AllowSelected`, разрешения по типам данных (Text/Images/Audio/Video) на провайдера. Любой облачный вызов проходит через одну функцию-шлюз, которая проверяет политику: скрытых вызовов нет by construction.
- AI не имеет доступа к filesystem/shell: единственный выход LLM — `EditCommand` из закрытого enum.
- Границы времени в командах AI — миллисекунды; `SelectCamera` притягивает их к краю клипа (в пределах полукадра) или к кадру, иначе при NTSC-частотах граница промахивается мимо клипа или отрезает осколок.

### 11.1 Решения Jev

```
JevItem { kind, subject, state, questions, primary }        ← templates.rs (shot_item / camera_item)
   → JevDecisionService::estimate  → карточка согласия (модель, токены, $, кэш) — тот же шлюз CostGuard + PrivacyPolicy
   → JevDecisionService::run
        ├─ кэш: ключ blake3(model + prompt_version + state + questions); попадание — бесплатно
        ├─ батчи: вопросы с {item}, id с префиксом i<j>.; 400 TooLarge → батч делится пополам
        └─ decided.rs: распределение → value, p_max, margin → Gate {AutoApply | Suggest | Review}
   → Project.jev_decisions (StoredDecision), гистерезис 0.05 против дребезга между запросами
```
- **Оценка кадров** (`ShotUsability`): KEEP / REVIEW / DISCARD по сводке анализа изображения и звука. Вердикты — метки на клипах, в инспекторе и в статусе библиотеки; ничего не удаляется. Щелчок по вердикту — исправление человека (`StoredDecision.human`) + `EditorPreferenceEvent` со ссылкой на решение.
- **Выбор камеры** (`CameraPick`): мультикам-клип режется на интервалы по 4 с, для каждого — признаки ракурсов (описание из группы, резкость, экспозиция, громкость). Результат — `Plan` из `SelectCamera` (всегда на просмотр, склейки не короче 2 с). UNSURE и Review-гейт оставляют текущий ракурс; если так везде, пользователь получает подсказку описать камеры, а не «Jev согласен».
- **Персонализация** (настройка «Учиться на моих правках», `AiSettings.jev_personalize`): ручное переключение ракурса внутри решённого интервала записывает `CameraChanged`. При следующем запросе `prefs::precedents` отбирает k похожих прошлых исправлений (похожесть × затухание по времени) и кладёт их в `state`; вопросы `editor_pick` + `has_precedent`. Если прецедент уместен (`has_precedent ≥ 0.5`), распределение Jev смешивается с частотным априором прошлых выборов: `blend(p_jev, p_freq, blend_weight(n))`.

## 12. Cache architecture

`%LOCALAPPDATA%\Kadr\cache\<asset-key>\{thumbs.bin, audio.pcm, peaks.bin, silence.json, meta.json}`
- `asset-key` = blake3(canonical path + size + mtime).
- Каждый артефакт хранит `algo_version`; несовпадение версии → пересчёт. Изменение файла → новый key.
- Запись артефакта атомарная (tmp + rename), недописанные файлы не используются.

## 13. Crash safety

- Сохранение: сериализация → `name.kadr.tmp` → fsync → rename (атомарно на NTFS в пределах тома) + `.bak` предыдущей версии.
- Autosave каждые 60 с при dirty в `name.kadr.autosave`; при открытии, если autosave новее — предложение восстановить.
- Логи: `%LOCALAPPDATA%\Kadr\logs\kadr.YYYY-MM-DD.log` (JSON). API-ключи никогда не логируются (тип `Secret` с redacted `Debug`).

## 14. Основные риски

| Риск | Митигация |
|---|---|
| Латентность seek через FFmpeg CLI (100–300 мс на 4K long-GOP) | coalescing запросов, preview в пониженном разрешении, V0.2: all-intra proxy + libav in-process декодер |
| A/V sync при потоковом preview через pipe | аудио — мастер-часы; видео-кадры пропускаются/повторяются по часам |
| Производительность Slint при больших таймлайнах | виртуализация, изображения waveform вместо Path |
| Точность времени/VFR | flicks + рациональные fps + тесты на все стандартные частоты |
| windows-gnu toolchain | переход на MSVC для release-сборок; код не зависит от toolchain |
| Стоимость/приватность AI | единый шлюз, бюджеты, явные подтверждения, локальные intents первыми |
| Лицензия FFmpeg (GPL-сборка) | используется как внешний процесс; для дистрибуции — LGPL-сборка или пользовательский FFmpeg |

## 15. Локализация (i18n)

- Все пользовательские строки — ключи в `crates/i18n/locales/{en,ru}.json`. Rust: `t()`, `tf()` (именованные `{param}`), `tn()` (плюрализация: ru one/few/many, en one/other). Slint: глобальный `Tr.t("key")`, `Tr.t1("key", a)`.
- Переключение языка мгновенное: `Tr.rev` — реактивная зависимость всех привязок; Rust-модели пересобираются `refresh_all()`.
- Движок и AI-слой не зависят от UI: команды отдают ключи (`cmd.*`, `err.edit.*`), бюджет/приватность — структурированные значения (`LimitHit`, `privacy.deny.*`), UI переводит их при показе.
- Тесты гарантируют: одинаковые наборы ключей и плейсхолдеров в обоих каталогах; каждый ключ, используемый в `.slint`/`.rs`, существует (`crates/i18n/tests/sources.rs`).

## 16. UX-решения (V0.1)

- Окно: разворачивается после показа (иначе Slint применяет preferred size), без ограничений max-size; тёмная системная рамка через DWM; адаптивная раскладка (боковые панели ужимаются до минимумов, превью переходит в компактный режим < 640 px, таймлайн ≤ 45 % высоты); раскладка панелей сохраняется.
- Собственное тёмное меню (нативное меню Win32 не темизируется) + горячие клавиши (F1 — справка), контекстные меню на клипах, дорожках, линейке, медиа, папках.
- Всплывающие уведомления (info/success/warning/error) вместо строки статуса; подтверждения для разрушающих действий (удаление медиа/папок, очистка кэша, выход без сохранения); Esc закрывает верхний оверлей.
- Экран приветствия с недавними проектами; индикатор сохранения («Сохранено в 14:32»); «грязность» по позиции в истории (Undo до сохранённого состояния = чистый документ).
- Inspector: точный ввод значений (двойной щелчок), сброс секций, панель последовательности и маркеров при пустом выделении.

## 17. Jev (TypeSafe) — выводы исследования (`mds/jev-research.md`)

- Модель решений, не чат: `POST https://api.typesafe.ai/v1/systemone`, вопросы `noul` / `choice` / `score`, ответ — распределения вероятностей. $0.042 / 1M входных токенов, выход бесплатный.
- Лимиты в токенах (≈32k state + вопрос, 64k запрос) → батчи делятся заранее, а не обрезаются. Ответы не детерминированы (±0.01–0.03) → локальный кэш решений.
- Персонализация — только in-context: история правок монтажёра (`EditorPreferenceEvent`) в `state` сильно смещает вероятности (подтверждено живыми вызовами). Пороги — по `p_max` и отрыву от второго варианта, всегда с вариантом REVIEW.
- Инструкции на английском, транскрипт — как есть; маршрутизация Jev — только рекомендация, бюджетные ограничители в коде.

## 18. Roadmap

- **V0.1** — см. ТЗ §26 (текущая работа).
- **V0.2** — Whisper (whisper.cpp), transcript editor, REST LLM, AI plan/review/apply, Jev, scene detection, multicam sync (кросс-корреляция огибающих аудио), video proxies, wgpu compositing.
- **V0.3** — multicam rough cut, AI Director, semantic search, voice, эффекты, captions, advanced export.

## 19. MCP-управление

Claude (Claude Code / Claude Desktop) может видеть и управлять Kadr через
Model Context Protocol. Полная документация для пользователя — `docs/mcp.md`;
здесь — архитектурная схема.

Два upstream-сервера объединены в один список инструментов, который отдаёт
`apps/kadr-mcp` по stdio:

```
Claude ──stdio MCP──► kadr-mcp.exe ──HTTP JSON-RPC 127.0.0.1:<port>, Bearer token──► kadr-mcp-bridge (UI-поток Kadr)
                          │                                                              ▲
                          └── ui_* ──HTTP 127.0.0.1:<SLINT_MCP_PORT>──► embedded MCP-сервер Slint (sight + real input) ┘
                          │
                          └── нет живого инстанса → запускает `kadr.exe --headless`, ждёт mcp.json
```

- `ui_*` — 1:1 прокси в MCP-сервер Slint 1.18 (`get_element_tree`,
  `take_screenshot`, `click_element`, `drag_element`, `dispatch_key_event`, …):
  снимки окна и реальный ввод через настоящую маршрутизацию событий Slint.
- Kadr-инструменты (`get_state`, `import_media`, `edit`, `undo`, `select`,
  `place_media`, `project`, `export`, `layout_text`, …) обслуживает
  `kadr-mcp-bridge`, встроенный в `apps/editor`.
- Файл обнаружения инстанса: `<data dir>/mcp/<pid>.json` — `AppDirs`
  (соответствует `KADR_DATA_DIR`, если задан), содержит `port`, `token`,
  `ui_port`, `pid`, `started_ms`, `headless`. `kadr-mcp` читает его, проверяет,
  что PID жив, и пингует; при отсутствии/устаревании — запускает
  `kadr.exe --headless` рядом с собой (или `KADR_EXE`) и ждёт файл до 15 с.
  Headless-инстанс, запущенный самим `kadr-mcp`, завершается по закрытию
  stdin (конец MCP-сессии).
- Контекстные меню рендерятся внутри Slint-окна (`SLINT_NO_MUDA=1`), поэтому
  видны в `ui_take_screenshot` и кликабельны через `ui_click_element`.
- Настройка «Разрешить управление через MCP» (по умолчанию включена)
  выключает оба сервера — изменение вступает в силу после перезапуска Kadr,
  после которого без неё не экспортируется `SLINT_MCP_PORT` и не стартует
  `kadr-mcp-bridge`.
