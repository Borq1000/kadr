# Kadr — архитектура рендеринга (B → C)

Дата: 2026-09-30 · Статус: **утверждена** (с поправками владельца от 2026-09-30).
Связанный документ: [система эффектов](2026-09-30-effects-system-design.md).

Цель: фундамент, на котором редактор развивается последовательно —
CPU renderer → proxy → GPU renderer (wgpu) → hardware decode → GPU effects →
render cache → оптимизированный экспорт — без переписывания timeline и модели
проекта.

```
Timeline ─► Scene Evaluator ─► FrameScene (логическая сцена)
                                   │   ссылки: media + source time, без пикселей
                                   ▼
                 Playback / Media Resolver (kadr-playback)
                  декодеры, кэш кадров, прокси, отмена, prefetch
                                   │
                                   ▼
                 PreparedFrame = FrameScene + RenderInputs (кадры CPU | позже GPU)
                                   │
                                   ▼
                 Renderer (kadr-render: CpuRenderer, позже WgpuRenderer)
                                   │
                        ┌──────────┴──────────┐
                        ▼                     ▼
                 Preview (дисплей)      Export (encoder)
```

FFmpeg — только demux / decode / seek / encode / mux / hwaccel / форматы.
Композиция, трансформации, прозрачность, текст, переходы, анимация, эффекты,
цвет — собственный код Kadr.

---

## 1. Как устроено сейчас

### 1.1 Модель проекта (`crates/project`)
- `Sequence { frame_rate, width, height, tracks, transitions, markers, in_out }`;
  дорожки: сначала видео (V1 — нижняя), потом аудио.
- `Clip { asset, source_in/out, timeline_in/out, link, enabled, transform,
  color, audio, effects, keyframes, multicam }`; скорость — отношение длительностей.
- `Transform { x, y (смещение от центра, пиксели последовательности), scale,
  rotation_deg, crop_* (доли), opacity }`, `ColorAdjust { exposure, contrast,
  saturation, temperature }`.
- `effects: Vec<Effect { kind: String, params: BTreeMap }>`,
  `keyframes: Vec<Keyframe { property: String, … }>` — в данных есть, нигде не
  вычисляются.
- `Transition { kind, track, at (точка склейки), duration }` — отдельный список.
- Ассет — только медиафайл (`MediaKind::Video | Audio | Image`).

### 1.2 Timeline (`crates/timeline`)
`EditEngine` (undo/redo), команды, мультикамера; `composition.rs`:
`video_at` — **один** верхний клип; `video_segments` — **сплющивание** всех
видеодорожек в один ряд сегментов; `transitions_into`; `audio_segments`.

### 1.3 Media (`crates/media`)
- `trait MediaBackend { probe, decode_frame, open_stream, thumbnails,
  extract_pcm, export }`, реализация `FfmpegCli`: каждая операция — отдельный
  процесс.
- `open_stream(StreamRequest { path, start, width, height, rate, speed, look,
  px_scale })`: процесс ffmpeg `-ss … setpts, fps, scale+pad (letterbox),
  format=rgba` → rawvideo pipe → `RgbaFrame { width, height, data: Vec<u8> }`.
- **`look` и `px_scale` потоком игнорируются** (с V0.1, `9f36dfb`): превью не
  показывает трансформацию, цвет и прозрачность клипа.
- Матрица YUV→RGB и диапазон при конвертации не задаются явно; probe не читает
  `color_primaries/transfer/space/range` и SAR.
- `export.rs`: сплющенные сегменты → один `filter_complex` (`look_filter`,
  `concat`, `xfade`, аудио-фильтры) → `libx264` + AAC; число сегментов
  ограничено длиной командной строки Windows.

### 1.4 Preview (`apps/editor/src/preview.rs`)
Один поток; команды `Show/Play/Stop`, слияние очереди, счётчик поколений.
`Show` — процесс ffmpeg на каждый кадр паузы; `Play` — процесс на сегмент, без
подготовки следующего (заминки на склейках), темп по аудиочасам с выбросом
опоздавших. Кадр → UI-поток → `SharedPixelBuffer::clone_from_slice` →
`slint::Image`. Время внутри источника считается через `f64`
(`(t - start).as_secs_f64() * speed`).

### 1.5 Audio (`crates/audio`)
PCM-кэш → микшер (gain, pan, fades) → lock-free кольцо → cpal. Мастер-часы
воспроизведения: позиция = число сыгранных сэмплов → `Time::from_samples`
(точно). В экспорте та же обработка повторена фильтрами ffmpeg.

### 1.6 Прочее
Миниатюры (процесс на запрос), MCP `get_frame`, анализ видео (`open_stream`),
волны из PCM, `kadr-jobs` (пул задач), `kadr-cache` (производные данные).

---

## 2. Конфликты с целевой схемой

| # | Где | Конфликт |
|---|---|---|
| K1 | `composition.rs` | Модель «один верхний клип» — нет слоёв |
| K2 | `export.rs`, `preview.rs` | Два механизма композиции, уже разошлись (превью без `look` и переходов) |
| K3 | `kadr-media` | Семантика композиции в слое медиа (`VideoLook`, `look_filter`, `build_graph`) |
| K4 | `StreamRequest` | Декодер отдаёт скомпонованный кадр (letterbox, скорость) вместо исходного |
| K5 | `MediaBackend` | Смешаны декодирование, миниатюры, PCM и исполнение экспорта |
| K6 | preview worker | Один поток; процесс на кадр паузы; нет кэша, prefetch, параллельного декодирования слоёв |
| K7 | `Clip.effects/keyframes` | Строковые ключи без валидации |
| K8 | `Clip.asset` | Источник — только медиафайл (нужны генераторы: текст, цвет, фигура) |
| K9 | `RgbaFrame` | Нет stride, формата, цветовых метаданных, варианта GPU |
| K10 | цвет | Матрица/диапазон YUV→RGB неявные; метаданные не читаются |
| K11 | время | `f64`-арифметика в пути источник↔таймлайн превью и экспорта |
| K12 | аудио | Две реализации микширования (вне этого подпроекта) |

---

## 3. Crates и направление зависимостей

| Crate | Статус | Ответственность |
|---|---|---|
| `kadr-core` | есть, расширяем | `Time` (flicks), id, `FrameRate`, `CancelToken`; **+ `color`** (цветовые метаданные), **+ `frame`** (`CpuFrame`, `FramePool`, `PixelFormat`), **+ `perf`** (телеметрия) — чистый std, без зависимостей |
| `kadr-project` | есть | Сохраняемая модель. Позже: `ClipSource`, эффекты-рецепты, ключевые кадры |
| `kadr-timeline` | есть, расширяем | Редактирование, undo; **+ `scene`: Scene Evaluator** |
| **`kadr-scene`** | новый, **только данные** | `FrameScene`, `Layer`, `LayerContent`, `Placement`, `BlendMode`, `Effect` (примитивы), `RenderQuality`, `OutputSpec`, геометрия |
| **`kadr-render`** | новый | **`trait Renderer`**, `PreparedFrame`, `RenderInputs`, `RenderTarget`, `RenderStats`; `CpuRenderer`; позже `WgpuRenderer` (feature `wgpu`) |
| `kadr-media` | есть, рефакторинг | Декодер **исходных** кадров в запрошенном размере, энкодер, probe (с цветом и SAR), миниатюры, PCM |
| **`kadr-playback`** | новый | Resolver: сессии декодеров, кэш кадров, прокси, отмена, prefetch; `PreviewPlayer`; `ExportRunner`; сбор телеметрии |
| `apps/editor` | есть | UI, дисплей кадра, `SceneSource` поверх проекта, временный предпросмотр |

```
kadr-core ◄──── kadr-scene ◄──── kadr-render
   ▲  ▲              ▲                ▲
   │  └── kadr-media │                │
kadr-project         │                │
   ▲                 │                │
kadr-timeline ───────┘                │
   ▲                   kadr-playback ─┴──► kadr-scene, kadr-render, kadr-media, kadr-core
   │                          ▲
   └────── apps/editor ───────┘
```

Правила: `kadr-scene` — только данные и чистая геометрия; `kadr-render` не
знает timeline, проект и FFmpeg; `kadr-timeline` не знает о рендерере;
`kadr-playback` получает сцены через `trait SceneSource` и не зависит от
timeline; `kadr-media` не знает, что делают с кадрами.

---

## 4. Граница «сцена ↔ реальные кадры»

### 4.1 Логическая сцена (`kadr-scene`) — без пикселей

```rust
pub struct FrameScene {
    pub time: Time,                 // время таймлайна
    pub canvas: SizeU,              // логический холст = размер последовательности, квадратные пиксели
    pub output: OutputSpec,         // размер, качество, цветовое пространство вывода
    pub background: Rgba,           // значение в рабочем пространстве (§6)
    pub layers: Vec<Layer>,         // снизу вверх, уже после отсечения невидимого (§7.3)
}

pub struct Layer {
    pub id: LayerId,                // стабильный (id клипа / генератора): ключ кэшей и GPU-ресурсов
    pub content: LayerContent,
    pub placement: Placement,       // вычислен (анимация применена)
    pub crop: RectF,                // в локальном пространстве слоя (§5)
    pub opacity: f32,
    pub blend: BlendMode,
    pub effects: Vec<Effect>,       // только примитивы, по порядку
    pub motion: Option<MotionSample>, // позже: положения на открытии/закрытии затвора для motion blur
}

pub enum LayerContent {
    /// Ссылка на медиа, не кадр. Что именно будет декодировано (оригинал,
    /// прокси, размер, CPU/GPU) — решает resolver.
    Media { media: MediaRef, source_time: Time },
    Solid(Rgba),
    Transition { kind: TransitionOp, progress: f32, from: Vec<Layer>, to: Vec<Layer> },
    // позже: Text(TextSpec), Shape(ShapeSpec), Nested(Box<FrameScene>)
}

pub struct MediaRef { pub media: MediaKey /* = AssetId */, pub stream: u32, pub kind: MediaKind /* Video | Image */, pub display_size: SizeU /* после SAR и поворота */ }
pub struct OutputSpec { pub size: SizeU, pub quality: RenderQuality, pub color: ColorInfo }
pub enum RenderQuality { PreviewFast, PreviewHigh, Export }
```

### 4.2 Resolver (`kadr-playback`) — получение реальных кадров

```rust
pub struct FrameRequest { pub media: MediaKey, pub source_time: Time, pub decode_size: SizeU, pub repr: Representation }
pub enum Representation { Original, Proxy /* позже Optimized */ }   // Export → всегда Original

pub trait SceneSource: Send + Sync { fn scene_at(&self, t: Time, out: &OutputSpec) -> FrameScene; }
```

Resolver для каждого `Media`-слоя сцены:
1. считает **нужный размер декодирования**: занимаемая слоем площадь в
   выводе (`placement` × `output/canvas`), не больше исходного. Маленький
   слой «картинка в картинке» декодируется маленьким;
2. выбирает representation (оригинал/прокси) по назначению и настройкам;
3. берёт кадр из кэша или ставит запрос сессии декодера;
4. собирает `PreparedFrame`.

### 4.3 Подготовленный кадр и рендерер (`kadr-render`)

```rust
pub struct PreparedFrame<'a> { pub scene: &'a FrameScene, pub inputs: RenderInputs }
pub struct RenderInputs { pub layers: Vec<LayerInput> }        // параллельно scene.layers (рекурсивно для переходов)
pub enum LayerInput {
    Cpu(Arc<CpuFrame>),         // сейчас
    // Gpu(GpuFrame),           // позже: текстура / аппаратная поверхность
    Missing(MissingReason),     // оффлайн/ошибка → рендерер рисует заглушку, сцена не меняется
    None,                       // слой без внешнего входа (Solid, Transition)
}

pub trait Renderer {
    fn name(&self) -> &str;
    fn render(&mut self, frame: &PreparedFrame, target: &mut RenderTarget) -> Result<RenderStats, RenderError>;
}
pub enum RenderTarget<'a> { Cpu(&'a mut CpuFrame) /* , Gpu(…) позже */ }
```

Сцена не знает, какой вход у слоя; вход не знает, как слой нарисуют. Появление
`LayerInput::Gpu` добавляет один вариант и ветку в resolver и рендерере, не
трогая сцену, evaluator, timeline и проект.

### 4.4 Кадры (`kadr-core::frame`)

```rust
pub enum PixelFormat { Rgba8, Nv12 /* позже: P010, RgbaF16 */ }
pub struct CpuFrame { pub size: SizeU, pub stride: usize, pub format: PixelFormat, pub color: ColorInfo, pub data: PooledBuf }
pub struct FramePool { /* буферы по (формат, размер); PooledBuf возвращается при Drop */ }
```
`Arc<CpuFrame>` делят кэш, рендерер и дисплей без копий.

---

## 5. Координаты и трансформации (aspect-correct)

- **Пространство холста** — пиксели последовательности (`canvas`), пиксели
  квадратные. X и Y — одинаковые геометрические единицы. Поворот, равномерный
  масштаб, окружности, маски, траектории движения и текст задаются в этих
  единицах.
- **Локальное пространство слоя** — пиксели контента: прямоугольник
  `size = (w, h)` в единицах холста при масштабе 1. Для медиа `size` —
  вписывание (contain) **отображаемого** размера источника (с учётом SAR и
  поворота из метаданных) в холст. Нормализованные UV `[0..1]²` используются
  **только для выборки** из текстуры/кадра, никогда для геометрии.
- **Placement**:
  `canvas = position + R(rotation) · diag(scale.x, scale.y) · (local − anchor·size)`,
  `anchor` — нормализованная точка контента (0.5, 0.5 — центр), `position` — в
  пикселях холста. Поворот вокруг `anchor`.
- **Crop** — прямоугольник в локальном пространстве. Обрезанная часть не
  рисуется, оставшаяся **остаётся на месте**: неявного смещения и центрирования
  нет. Для перемещения — `placement`. Anchor по-прежнему относится ко всему
  контенту.
- **Вывод**: `output` имеет пропорции холста (иначе холст вписывается с
  полями). Масштаб холст → вывод равномерный. Эффекты с радиусом (blur и т. п.)
  задаются в пикселях холста и масштабируются к выводу, поэтому при превью ½
  выглядят так же.
- **Миграция `Transform`**: `position = center + (x, y)`, `anchor = (0.5, 0.5)`,
  `scale = (s, s)`, `rotation = deg.to_radians()`, `crop_*` → прямоугольник
  crop. Старое поведение crop (перецентрирование) **не сохраняем**: проекты с
  обрезкой откроются с новой, правильной семантикой.

---

## 6. Цвет

- **Метаданные есть в контракте сразу** (`kadr-core::color`):
  ```rust
  pub struct ColorInfo { pub primaries: Primaries, pub transfer: Transfer, pub matrix: Matrix, pub range: Range, pub alpha: AlphaMode }
  // Primaries: Bt709 | Bt601_625 | Bt601_525 | Bt2020
  // Transfer:  Bt709 | Srgb | Linear | Pq | Hlg
  // Matrix:    Rgb | Bt709 | Bt601 | Bt2020Ncl
  // Range:     Limited | Full
  // AlphaMode: Opaque | Straight | Premultiplied
  ```
  Probe читает `color_primaries/transfer/space/range` и SAR; неизвестные
  значения — правило: ширина ≥ 1280 или высота > 576 → BT.709; высота 576 → BT.601/625; иначе BT.601/525; диапазон Limited. Форматы семейства RGB (rgb*, bgr*, argb, abgr, 0rgb, 0bgr, gbr*, x2rgb10*, x2bgr10*, pal8) без метки `color_space` — матрица `Rgb`, диапазон Full. Альфа видео — по формату пикселей: `Opaque` только для заведомо непрозрачных форматов (yuv* кроме yuva*, nv12, p010, gray*, rgb24, bgr0, gbrp* …), всё остальное, включая неизвестные и пустые имена, — `Straight` (ошибка стоит лишнего декодирования, но не неверного кадра); метка `alpha_mode=1` — тоже `Straight`.
- **Первая поддерживаемая конфигурация — SDR Rec.709.** Декодер конвертирует
  YUV→RGB **с явно заданными** матрицей и диапазоном источника в Full-range
  R'G'B' (нелинейные, «гамма-кодированные» значения). HDR, float-конвейер и
  управление цветом — не сейчас.
- **Рабочее пространство SDR-рендерера (фиксируем для CPU и GPU одинаково):**
  - значения — нелинейные Rec.709 R'G'B' (display-referred), 8 бит на канал;
  - альфа **premultiplied**;
  - смешивание слоёв и билинейная выборка — над **нелинейными** значениями
    (как 8-битный режим Vegas и режим по умолчанию Premiere);
  - цветовые операции — над **непремультиплицированным** цветом
    (разделить на α → операция → умножить на α) по формулам из
    [спецификации эффектов](2026-09-30-effects-system-design.md#примитивы-цвета);
  - округление float → u8: к ближайшему, половина — вверх;
  - будущий GPU-рендерер использует текстуры `Rgba8Unorm` (**не** `*Srgb`: иначе
    выборка и смешивание станут линейными и разойдутся с CPU) и те же формулы
    в шейдерах; допуск паритета CPU/GPU — ±1 LSB на канал.
  - Линейное рабочее пространство — будущий явный режим для обоих рендереров.
- **Тесты цвета** (минимальные): (1) эталонные полосы (smptebars) в
  H.264 Rec.709 Limited → декодированные значения RGB в пределах ±2 от
  эталона; (2) тот же тест для SD Rec.601; (3) `saturation = 0` даёт серый с
  яркостью по весам Rec.709; (4) 50 % красного поверх синего даёт точное
  ожидаемое значение; (5) непрозрачный слой с `opacity = 1` не меняет пиксели.

---

## 7. Качество, стоимость и невидимая работа (крючки для системы эффектов)

### 7.1 Уровни качества
`RenderQuality::{PreviewFast, PreviewHigh, Export}` — часть `OutputSpec`.
Правило: **визуальная модель одна**. Качество меняет только точность
приближения (число сэмплов blur и motion blur, внутреннее уменьшение для
bloom), но не смысл параметров. Параметры эффектов — в пикселях холста.

### 7.2 Стоимость
Каждый примитив даёт оценку стоимости
`fn cost(&self, pixels: u64, quality) -> Cost` (Cheap / Medium / Heavy +
числовая оценка). Сумма по сцене нужна будущему адаптивному превью
(см. документ эффектов). Сейчас — только интерфейс и замеры, без
планировщика.

### 7.3 Не выполнять невидимую работу
Evaluator не выпускает в сцену:
- слои с `opacity = 0` или с нулевым масштабом;
- слои целиком вне холста (по ограничивающему прямоугольнику после placement);
- слои под непрозрачным слоем, закрывающим весь холст (Normal-blend, без
  прозрачности, без crop, покрывает холст);
- выключенные эффекты и эффекты с нулевой силой.

Resolver декодирует **только** слои, дошедшие до сцены. Рендерер
дополнительно пропускает пустые пересечения и использует быстрые пути
(непрозрачный полнокадровый слой без трансформации — копия строк).

### 7.4 Проходы и промежуточные буферы
Примитивы делятся на:
- **точечные** (цвет, кривая, виньетка, зерно, opacity) — сливаются в один
  проход вместе с выборкой слоя;
- **геометрические** (placement, crop) — часть выборки;
- **окрестностные** (blur, bloom, motion blur) — требуют промежуточного буфера.

Промежуточные буферы — только для окрестностных эффектов и переходов, из пула.
Слой без них рисуется прямо в цель.

---

## 8. Время и синхронизация

- Внутри — только `Time` (i64 flicks, 1/705 600 000 с) и целые номера кадров
  через рациональный `FrameRate`. Отображение таймлайн → источник — точное
  (`Clip::source_time_at`, `mul_ratio`). **Никакого накопления f32/f64
  секунд**; `f64` допускается только на границах (аргументы ffmpeg, UI).
- Мастер-часы воспроизведения — аудио (число сыгранных сэмплов → `Time`); без
  аудиоустройства — стенные часы. Видео подгоняется к ним; опоздавшие кадры
  выбрасываются и учитываются в телеметрии.
- **Тесты A/V-синхронизации**:
  - экспорт синтетического длинного файла (20 мин, вспышка + щелчок каждую
    секунду) после ≥ 50 склеек и ≥ 10 переходов: начало щелчка совпадает со
    вспышкой в пределах ±1 кадра по всему файлу;
  - воспроизведение в превью с тестовыми часами: расхождение показанного
    кадра и аудиочасов не выходит за 1 кадр за 10 минут симуляции.

---

## 9. Декодеры (`kadr-playback`)

- **Пул долгоживущих сессий декодирования.** Сессия — контекст декодирования
  источника (media, representation, размер) с текущей позицией. Сколько сессий
  на источник и когда их открывать, продлевать, гасить, решает пул по
  текущему времени, активным слоям и одновременным запросам. Жёсткого правила
  «один процесс на дорожку» нет.
- Запрос чуть впереди позиции сессии — дочитываем; иначе — seek (сейчас это
  перезапуск процесса ffmpeg).
- **Scrubbing**: новый запрос повышает поколение; устаревшие запросы
  отбрасываются, не дожидаясь декода; кадр «сейчас» — высший приоритет,
  соседи — фоновый prefetch.
- **Playback**: упреждающее чтение на N кадров на каждый активный слой;
  следующий клип открывается заранее.
- **Кэш кадров**: LRU по байтам, ключ (media, repr, размер, номер кадра).
- Реализация сессии сейчас — процесс FFmpeg CLI, пишущий в пулованные буферы.
  Интерфейс сессии не предполагает CPU-кадр как единственный результат.

---

## 10. Телеметрия (`kadr-core::perf`)

```rust
pub struct FramePerf {
    pub total: Duration, pub decode: Vec<(LayerId, Duration)>,
    pub evaluate: Duration, pub resolve: Duration, pub upload: Duration,
    pub composite: Duration, pub effects: Duration, pub present: Duration,
    pub cache_hits: u32, pub cache_misses: u32, pub dropped: u32,
    pub seek_latency: Option<Duration>, pub frame_allocs: u32, pub frame_copies: u32,
}
```
Кольцевой буфер в `kadr-playback`; DEV-оверлей в углу превью; MCP-инструмент
`get_perf`. Каждый этап миграции заканчивается числами отсюда.

---

## 11. Лишние копии больших кадров

| # | Место | Сейчас | Цель |
|---|---|---|---|
| C1 | `FfmpegStream::next_frame` | выделение + обнуление на каждый кадр | `FramePool`, без обнуления |
| C2 | pipe ffmpeg → Kadr | 2 копии через ядро | неизбежно для CLI |
| C3 | `on_preview_frame` | `clone_from_slice` 8 МБ **на UI-потоке** на кадр | рендер в буфер, отдаваемый дисплею; копия с UI-потока убрана |
| C4 | Slint | загрузка новой `Image` в текстуру на кадр | неизбежно на CPU-пути; на GPU-пути — без копии (§12) |
| C5 | `RgbaFrame::black` | выделение на кадр пропуска | чёрный рисует рендерер (фон) |
| C6 | `parse_pam` | `to_vec()` | срез владельца / пул |
| C7 | `derive(Clone) RgbaFrame` | клон = копия | `Arc`-кадры |
| C8 | RGBA через pipe | 4 байта/пиксель (4K×30 ≈ 1 ГБ/с на слой) | NV12 (1,5 байта) при GPU-конвертации; на CPU — по замеру |
| C9 | CPU-композитинг | риск промежуточного буфера на слой | прямо в цель (§7.4) |

---

## 12. Путь к GPU и hardware decode

1. **Slint + wgpu (проверено по исходникам Slint 1.18).**
   - `BackendSelector::require_wgpu_30(WGPUConfiguration::Manual { instance, adapter, device, queue })` — Slint рендерит окно на **нашем** Device/Queue.
   - `Image::try_from(wgpu::Texture)` показывает текстуру без копии; формат `Rgba8Unorm`/`Rgba8UnormSrgb`, usage `TEXTURE_BINDING | RENDER_ATTACHMENT`.
   - Rendering notifier отдаёт `GraphicsAPI::WGPU30 { instance, device, queue }`.

   Значит, представление без копии принципиально возможно. Условия и риски:
   - API помечен `unstable` и привязан к версии wgpu (Slint 1.18 ↔ wgpu 29/30), при обновлении Slint нужна синхронизация;
   - окно переключается на wgpu-рендерер Slint (femtovg-wgpu или skia-wgpu) — проверить вид и производительность UI;
   - финальный кадр превью — `Rgba8Unorm` (совпадает с §6).
2. **Гибридный ноутбук (Intel + NVIDIA).** Адаптер выбираем сами (NVIDIA для
   рендера) и передаём Slint через `Manual`: UI и рендер на одном устройстве,
   без межадаптерных копий. Экспорт с NVENC — на том же устройстве.
3. **Hardware decode.** Процесс FFmpeg CLI отдаёт кадры только в RAM, поэтому
   поверхности GPU без копии через CLI невозможны. Промежуточный путь —
   hwdecode → NV12 в RAM → загрузка в текстуру → конвертация в шейдере.
   Zero-copy требует in-process декодирования. Архитектура это допускает:
   `LayerInput::Gpu`, сессия декодера, `ColorInfo`.
4. **Цвет на GPU**: NV12/P010 + `ColorInfo` → конвертация в шейдере по тем же
   формулам, что у декодера CPU-пути; проверяется тестами цвета (§6).

**Лицензирование FFmpeg** (LGPL/GPL, распространение кодеков) — отдельное
будущее исследование, не входит в архитектурные решения рендеринга. Сейчас
FFmpeg CLI — безопасный технический этап.

---

## 13. Что делаем сейчас, что закладываем

**Подпроект 1 (реализуем):**
- `kadr-core::{color, frame, perf}`; probe с цветом и SAR; декодер с явной
  матрицей и диапазоном, выдающий исходные кадры в пулованные буферы.
- `kadr-scene`: сцена, слои `Media` (видео и изображения) / `Solid` /
  `Transition`, `Placement`, crop, opacity, `BlendMode::{Normal, Add, Multiply,
  Screen}`, `Effect::ColorAdjust`, `RenderQuality`, `OutputSpec`.
- Evaluator: многодорожечная композиция, переходы (наплыв, через чёрное,
  шторка), отсечение невидимого.
- `CpuRenderer`: premultiplied RGBA8, аффинная выборка (билинейная), быстрые
  пути, rayon, переиспользование буферов, стоимость примитивов.
- `kadr-playback`: resolver, пул сессий, кэш, отмена, prefetch,
  `PreviewPlayer`, `ExportRunner` → энкодер ffmpeg через stdin (rawvideo);
  звук экспорта — существующим аудиографом ffmpeg.
- Превью и экспорт на новом конвейере; старые пути удалены после приёмки.
- Телеметрия, DEV-оверлей, `get_perf`; эталонные тесты (alpha,
  многослойность, трансформации, crop, переходы, цвет); тесты A/V; замеры
  до и после.

**Закладываем (документом и точками расширения, без пустого кода):**
текст, фигуры, вложенные сцены, эффекты кроме `ColorAdjust`, motion blur, маски,
ключевые кадры и рецепты (см. документ эффектов), прокси и render cache,
`LayerInput::Gpu`, `WgpuRenderer`, NV12 на GPU, in-process hw decode, линейное
рабочее пространство, адаптивное превью.

---

## 14. Миграция (программа работает на каждом шаге)

Этапы M0–M6 и их измеримые критерии выхода — в плане реализации
`docs/superpowers/plans/2026-09-30-render-foundation.md`. Порядок:
M0 замеры → M1 сцена и evaluator → M2 CpuRenderer → M3 playback → M4 превью на
новом конвейере → M5 экспорт → M6 удаление старых путей. К GPU и эффектам —
только после M6 и корректного, измеренного многослойного CPU-конвейера.

## 15. C++ / FFI

В подпроекте 1 не нужен. В будущем — только после замера и проверки
альтернатив (алгоритм, аллокации, потоки, SIMD, GPU, Rust-библиотеки).
Граница — крупнозернистая (`process_frame(input, output, params)`), буферы
принадлежат Rust, графов объектов через ABI нет.
