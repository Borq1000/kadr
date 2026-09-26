# ЗАДАЧА: Native AI Video Editor на Rust

Ты работаешь как senior Rust/C++ desktop engineer, video-processing engineer, GUI architect и AI systems architect.

Необходимо спроектировать и реализовать с нуля полноценное desktop-приложение для профессионального видеомонтажа с AI-first архитектурой.

Это не демонстрация и не web-приложение в desktop-обёртке.

Нужен настоящий нативный видеоредактор для Windows с красивым современным профессиональным интерфейсом, timeline, preview, многодорожечным монтажом, аудио, эффектами, экспортом и дополнительным AI-слоем, позволяющим управлять монтажом естественным языком.

Основной язык приложения:

**Rust**

Целевая ОС первой версии:

**Windows 10/11 x64**

Архитектура должна заранее позволять последующую поддержку macOS/Linux.

---

# 1. ОСНОВНЫЕ ПРИНЦИПЫ

Приложение должно оставаться полноценным видеоредактором даже при:

- отсутствии интернета;
- отсутствии API-ключей;
- отключённом AI;
- недоступности внешнего AI-провайдера.

AI является дополнительным интеллектуальным слоем над редактором, но не фундаментом его работы.

Основные операции монтажа должны быть детерминированными.

LLM никогда непосредственно не изменяет проект или видеофайлы.

LLM предлагает структурированные Edit Commands.

Команды:

1. валидируются;
2. показываются пользователю при необходимости;
3. исполняются Rust Edit Engine;
4. могут быть отменены через Undo;
5. записываются в историю проекта.

Исходные media-файлы никогда не модифицируются.

Монтаж полностью non-destructive.

---

# 2. НИКАКОГО WEB UI

НЕ использовать:

- React;
- Next.js;
- Electron;
- Tauri WebView;
- HTML/CSS UI;
- браузерный frontend.

GUI должен быть нативным.

Основной кандидат:

**Slint + Rust**

Перед реализацией исследуй актуальную документацию Slint и его текущие возможности.

Если существуют серьёзные технические причины выбрать другую Rust-native GUI библиотеку, сначала аргументированно сравни варианты:

- Slint;
- egui;
- iced;
- другие production-ready Rust GUI frameworks.

Но при отсутствии объективных препятствий использовать Slint.

Интерфейс должен выглядеть как коммерческое профессиональное приложение, а не developer tool.

Ориентиры по уровню UX:

- DaVinci Resolve;
- Final Cut Pro;
- Premiere Pro;
- современные профессиональные creative tools.

Не копировать их дизайн буквально.

Создать собственную визуальную систему.

Требования:

- dark UI;
- аккуратная типографика;
- минимальный визуальный шум;
- качественные hover/focus/selected states;
- плавные анимации там, где они оправданы;
- draggable panels;
- resizable panels;
- контекстные меню;
- keyboard shortcuts;
- HiDPI;
- multi-monitor readiness;
- профессиональная timeline UX.

---

# 3. ОСНОВНОЙ UI

Главное окно примерно разделяется на:

## Media Library

Импортированные:

- video;
- audio;
- images.

Показывать:

- thumbnails;
- duration;
- resolution;
- FPS;
- codec;
- audio information;
- proxy status;
- analysis status.

Поддержать folders/bins.

---

## Video Preview

Большой realtime preview.

Необходимо:

- play/pause;
- frame stepping;
- scrubbing;
- current timecode;
- zoom;
- fullscreen preview;
- before/after effects;
- safe areas;
- selectable preview quality.

---

## Timeline

Это одна из наиболее важных частей проекта.

Поддержать:

- unlimited conceptual tracks;
- video tracks;
- audio tracks;
- clip dragging;
- trim;
- split;
- ripple delete;
- insert;
- overwrite;
- snapping;
- linked audio/video;
- track mute;
- track solo;
- track lock;
- markers;
- zoom;
- horizontal scrolling;
- selection;
- multi-selection;
- transitions;
- basic keyframes.

Timeline должна оставаться отзывчивой на больших проектах.

Не строить timeline как набор тысяч тяжёлых GUI widgets.

Использовать virtualized/custom rendering там, где это необходимо.

---

## Inspector

Для выбранного clip:

Transform:

- position;
- scale;
- rotation;
- crop;
- opacity.

Video:

- speed;
- basic color controls;
- stabilization placeholder/interface.

Audio:

- gain;
- fade;
- pan.

Effects.

---

## AI Assistant

Отдельная collapsible/resizable панель.

Она не должна мешать обычному монтажу.

Пользователь может написать:

"Удали последние пять секунд."

"Разрежь здесь."

"Удали паузы длиннее двух секунд."

"Найди момент, где начинается второй номер."

"Оставь только лучшие моменты этого выступления."

"Сделай предварительную нарезку концерта."

"На этом участке чаще используй крупные планы."

AI должен учитывать:

- current project;
- current timeline;
- selected clips;
- playhead position;
- In/Out range;
- metadata;
- transcript;
- media analysis.

---

# 4. MEDIA ENGINE

Не писать собственный video codec.

Использовать FFmpeg там, где это разумно.

Создать Rust abstraction layer над media backend.

Media Engine должен обеспечивать:

- probing;
- decoding;
- seeking;
- thumbnail generation;
- waveform generation;
- audio extraction;
- proxy generation;
- rendering;
- encoding;
- muxing.

Не привязывать бизнес-логику напрямую к CLI-вызовам FFmpeg.

Создать чистый интерфейс MediaBackend, чтобы реализацию впоследствии можно было менять.

Изучи варианты:

- FFmpeg CLI;
- ffmpeg-next / bindings;
- libav\*;
- GStreamer, если он даёт объективные преимущества для определённой части pipeline.

Выбери архитектуру после исследования.

---

# 5. GPU

Исследовать использование:

**wgpu**

для:

- realtime compositing;
- transformations;
- transitions;
- color operations;
- effects;
- preview rendering.

Не использовать GPU просто ради использования.

Отделить:

Media Decode

от

Timeline Composition

от

Preview Rendering

от

Final Export.

---

# 6. PROJECT MODEL

Создать собственный сериализуемый project format.

Например:

.project / JSON + auxiliary database/cache.

Проект должен содержать ссылки на исходные media, но не сами media.

Основные сущности:

Project
MediaAsset
Sequence
Track
Clip
AudioClip
Transition
Effect
Keyframe
Marker
Transcript
AnalysisResult
EditOperation
AIAction
EditorCorrection

Каждый Clip должен ссылаться на MediaAsset и иметь:

source_in
source_out
timeline_in
timeline_out

Никакого destructive editing.

---

# 7. UNDO / REDO

Архитектура Command Pattern.

Каждая операция:

SplitClip
TrimClip
MoveClip
DeleteClip
InsertClip
ChangeProperty
AddTransition
AIEditBatch

должна быть обратимой.

AI-команда может содержать десятки операций, но для пользователя должна иметь возможность:

**Undo AI edit**

одним действием.

---

# 8. BACKGROUND JOB SYSTEM

Тяжёлые операции никогда не блокируют UI.

Создать background job architecture для:

- proxies;
- thumbnails;
- waveform;
- transcription;
- computer vision;
- AI requests;
- rendering;
- export.

Нужны:

- queue;
- progress;
- cancellation;
- priority;
- error state;
- retry.

Использовать Rust async/concurrency осмысленно.

Рассмотреть Tokio там, где async действительно нужен.

---

# 9. LOCAL ANALYSIS — БЕЗ API

Максимум анализа выполнять бесплатно локально.

После импорта видео программа должна уметь вычислять или иметь архитектурные модули для:

## Video

- scene boundaries;
- black frames;
- frozen frames;
- brightness;
- blur/sharpness;
- camera motion;
- approximate shake;
- shot duration;
- visual similarity;
- duplicate/near-duplicate shots.

## Faces

- face presence;
- number of faces;
- bounding boxes;
- face visibility;
- tracking.

Не обязательно реализовывать identity recognition.

## Audio

- waveform;
- silence;
- loudness;
- clipping;
- speech/non-speech;
- music/non-music;
- applause where practical;
- beats/BPM where practical.

Сначала использовать deterministic algorithms и локальные ML-модели.

Не отправлять данные в облако, если задача разумно решается локально.

---

# 10. TRANSCRIPTION

Создать SpeechToText abstraction.

Предусмотреть:

- local Whisper/whisper.cpp;
- external REST providers.

Результат:

word-level timestamps, если доступны;

speaker information, если доступна;

segments;

confidence.

Transcript должен быть связан с timeline.

Пользователь должен иметь возможность редактировать видео через текст transcript:

удалил предложение из transcript → соответствующий clip/range может быть удалён из timeline после подтверждения.

---

# 11. AI ARCHITECTURE

Не привязывать программу к OpenAI или Anthropic.

Создать общий:

AIProvider

с реализациями через REST.

Например:

OpenAIProvider
AnthropicProvider
JevProvider
CustomOpenAICompatibleProvider
LocalProvider

API keys хранятся безопасно.

Никогда не hardcode API keys.

Использовать системное защищённое хранилище credentials Windows, где возможно.

HTTP:

Rust + reqwest.

Нужны:

- timeout;
- retry;
- cancellation;
- rate-limit handling;
- structured errors;
- token accounting;
- cost accounting.

---

# 12. УРОВНИ AI

Приложение должно явно разделять:

## LOCAL

Стоимость:

$0

Используются локальные алгоритмы и модели.

---

## ECONOMY

Очень дешёвые API-модели и Jev.

Используются для массовой классификации/ranking.

---

## SMART

Обычная LLM для более сложного анализа.

---

## DIRECTOR

Сильная reasoning-модель.

Используется только для задач, где действительно требуется понимание всей истории/композиции.

Например:

"Из этих 90 минут материала собери хороший 12-минутный фильм о концерте."

---

# 13. JEV

Jev НЕ является vision model.

Не отправлять ему raw video.

Использовать Jev после локального извлечения признаков.

Создать JevDecisionService.

Потенциальные задачи:

## Shot usability

На основании:

- blur;
- shake;
- faces;
- obstruction;
- composition metadata;
- speech;
- uniqueness;
- neighbouring shots.

Решение:

KEEP / DISCARD / REVIEW

с confidence.

---

## Camera ranking

Для multicam:

CAM_A
CAM_B
CAM_C

выбрать предпочтительную камеру для данного временного интервала.

---

## Segment classification

Классы:

ESTABLISHING
MAIN_ACTION
CLOSE_UP
REACTION
B_ROLL
TRANSITION
LOW_VALUE
DISCARD

---

## Speech cleanup

После transcription:

- useful speech?;
- false start?;
- filler?;
- repeated idea?;
- removable without semantic loss?;
- completed thought?

---

## Duplicate semantic content

После предварительного поиска похожих transcript segments через embeddings:

Jev определяет, являются ли два фрагмента фактически повторением одной мысли.

---

## AI routing

Jev может использоваться как decision/router:

LOCAL
ECONOMY_MODEL
SMART_MODEL
DIRECTOR_MODEL

Но routing должен иметь deterministic safeguards.

Не разрешать Jev самостоятельно инициировать дорогие API-вызовы без установленных пользователем правил бюджета.

---

# 14. AI COST CONTROL

Это критически важная часть продукта.

Пользователь всегда должен понимать:

**используется ли сейчас внешний AI;**

**какой provider;**

**какая модель;**

**сколько примерно будет стоить операция;**

**сколько уже потрачено.**

В status bar:

AI: LOCAL

или:

AI: OpenAI / model-name

Session cost: $0.083

Project cost: $1.27

---

Перед потенциально дорогой операцией показывать:

AI Director Analysis

Media duration: 1h 47m

Provider: ...

Model: ...

Estimated input: ...

Estimated cost: $X–Y

[Run]

[Use cheaper model]

[Local only]

---

Настройки бюджета:

Per request limit
Per session limit
Per project limit
Monthly limit

При превышении лимита операция запрещается без явного подтверждения.

---

# 15. НИКАКИХ СКРЫТЫХ AI-ВЫЗОВОВ

Очень важное требование.

Программа не должна незаметно отправлять видео, изображения, transcript или другую информацию стороннему API.

В настройках:

AI OFF

Local only

Ask before cloud processing

Allow selected providers

Можно отдельно разрешать:

Text
Images
Audio
Video

для каждого provider.

---

# 16. EDIT COMMAND LANGUAGE

Создать строгую внутреннюю схему AI-команд.

LLM не должна возвращать свободный текст, который затем каким-либо образом "исполняется".

Пример концепции:

SplitClip
DeleteRange
TrimClip
MoveClip
InsertClip
SelectCamera
AddTransition
SetTransform
ChangeSpeed
SetAudioGain
AddMarker
AddCaption

Каждая команда:

- schema validated;
- range validated;
- permission checked;
- project-state checked.

Пример:

{
"type": "delete_range",
"sequence_id": "...",
"start_ms": 12500,
"end_ms": 16800,
"ripple": true,
"reason": "long silence"
}

AI не имеет произвольного доступа к filesystem/shell через эту систему.

---

# 17. AI PLAN / PREVIEW

Для больших AI-операций использовать двухфазную модель.

### PLAN

AI говорит:

- что обнаружено;
- что предлагается сделать;
- сколько операций;
- estimated cost;
- confidence.

Например:

"Обнаружено 43 длинные паузы.

Предлагаю удалить 37.

6 оставлены, поскольку они находятся возле аплодисментов/смены номера."

Пользователь:

Apply

Review

Cancel

### EXECUTE

Только после этого Edit Engine изменяет timeline.

---

# 18. MULTICAM

Архитектура должна с самого начала учитывать multicamera editing.

Это особенно важно для концертных видео.

Импорт:

CAM1
CAM2
CAM3
CAM4
external audio

Автоматическая синхронизация:

- audio waveform;
- timecode, если имеется;
- manual sync fallback.

Создать MulticamGroup.

Все камеры сохраняются.

Timeline хранит выбор активной камеры по временным диапазонам.

Пользователь может заменить камеру без перестройки монтажа.

---

# 19. AI MULTICAM ASSISTANT

Будущий/экспериментальный модуль должен анализировать:

- visibility;
- shot size;
- faces;
- motion;
- composition;
- technical quality;
- previous camera;
- duration since last cut;
- stage action;
- transcript;
- applause/music structure.

И создавать первоначальный multicam cut.

Не делать правило:

"всегда выбирай технически лучший кадр".

Художественный монтаж требует разнообразия.

Например, идеально резкий общий план не всегда лучше эмоционального close-up.

---

# 20. PERSONAL EDITOR PROFILE

С первого дня журналировать взаимодействие AI и человека.

Например:

AI предложил CAM2.

Human заменил CAM2 → CAM3.

Сохранить:

- context;
- features;
- AI choice;
- confidence;
- human correction.

Аналогично:

AI deleted segment → Human restored.

AI kept segment → Human deleted.

AI transition → Human changed transition.

Создать EditorPreferenceEvent.

Эти данные НЕ отправлять автоматически наружу.

В будущем они могут использоваться для:

- preference rules;
- Jev decisions;
- local ranking model;
- personalized editing model.

---

# 21. КОНЦЕРТНЫЙ WORKFLOW

Один из главных реальных use cases:

профессиональный монтаж концертов.

Пример:

3 камеры × 90 минут

- отдельная аудиозапись.

Workflow:

Import

→ automatic sync

→ proxies

→ scene/audio analysis

→ waveform

→ optional transcription

→ detect performances/sections

→ build multicam group

→ AI-assisted initial camera selection

→ human correction

→ transitions/color/audio

→ export.

Цель AI:

НЕ создать магически идеальный фильм.

Цель:

сократить огромный объём механической работы и дать человеку хороший editable first cut.

---

# 22. VOICE CONTROL

Архитектурно предусмотреть голосовые команды.

Speech:

"Разрежь здесь."

"Последние десять секунд удали."

"Верни."

"Отсюда до следующего номера используй в основном вторую камеру."

"Найди выступление Маши."

Voice → STT → same AI/Edit Command system.

Не создавать отдельный монтажный механизм для voice.

---

# 23. CACHE

Создать надёжный cache system.

Не пересчитывать:

- thumbnails;
- proxies;
- waveform;
- transcript;
- embeddings;
- scene analysis;
- vision analysis

после каждого открытия проекта.

Cache должен иметь versioning и invalidation.

---

# 24. PERFORMANCE

Проект должен проектироваться для реальных файлов:

1080p
4K
long-form footage
multiple cameras.

UI не должен зависать при:

- импорте;
- генерации thumbnails;
- waveform;
- analysis;
- export.

Использовать proxies.

Не держать целое видео в RAM.

Следить за memory allocation.

Профилировать реальные bottlenecks.

---

# 25. CRASH SAFETY

Нужны:

autosave;

project recovery;

atomic project writes;

background job recovery where reasonable;

logs.

После crash пользователь не должен потерять час монтажа.

---

# 26. ПЕРВАЯ РАБОЧАЯ ВЕРСИЯ

Не пытайся сразу реализовать весь документ.

Первый milestone должен дать реально используемый редактор.

## V0.1

Обязательно:

1. Native Slint application.
2. Premium dark interface.
3. Media import.
4. Video preview.
5. Audio playback.
6. Timeline.
7. One or more video/audio tracks.
8. Drag clips.
9. Trim.
10. Split.
11. Delete.
12. Ripple delete.
13. Playhead.
14. Timeline zoom.
15. Basic snapping.
16. Undo/redo.
17. Save/load project.
18. Basic FFmpeg export.
19. Background thumbnail generation.
20. Waveform.
21. Basic local silence detection.
22. AI provider architecture.
23. AI cost architecture.
24. AI panel.
25. One real natural-language operation:

"Удалить паузы длиннее N секунд."

При возможности эта конкретная операция сначала должна работать **полностью локально без LLM**.

Это важно:

AI UX не означает, что каждая естественная команда обязана вызывать LLM.

Если intent можно надёжно распознать локально, используй локальную реализацию.

---

# 27. V0.2

После стабильной V0.1:

- Whisper/local transcription;
- transcript editor;
- text-based editing;
- REST LLM integration;
- structured EditCommands;
- AI plan/review/apply;
- Jev integration;
- scene detection;
- multicam synchronization prototype.

---

# 28. V0.3

- automatic multicam rough cut;
- AI Director;
- semantic search;
- editor preference logging;
- voice commands;
- effects;
- transitions;
- captions;
- advanced export.

---

# 29. TESTING

Не ограничиваться unit tests.

Нужны:

Unit tests
Integration tests
Project serialization tests
Undo/redo tests
Edit command tests
Timeline arithmetic tests
FFmpeg integration tests.

Особенно тщательно проверить time calculations.

Не использовать floating point seconds как главный внутренний формат timeline.

Использовать integer time representation:

microseconds/nanoseconds либо рациональные media timestamps/timebase.

Корректно учитывать:

23.976
24
25
29.97
30
50
59.94
60 FPS

и variable frame rate media.

---

# 30. ЛОГИРОВАНИЕ

Использовать structured logging.

Debug information должно позволять понять:

- decoder errors;
- FFmpeg failures;
- AI errors;
- API responses;
- invalid EditCommands;
- cache problems;
- GPU errors.

Не записывать API keys или чувствительные данные в logs.

---

# 31. CODE QUALITY

Не создавать giant files.

Использовать понятные Rust crates/modules.

Предполагаемая workspace architecture может быть примерно:

apps/editor

crates/core
crates/project
crates/timeline
crates/media
crates/render
crates/audio
crates/analysis
crates/transcription
crates/ai
crates/jev
crates/jobs
crates/cache
crates/platform
crates/ui

Но это НЕ жёсткое требование.

Сначала спроектируй наиболее разумную структуру.

Core domain не должен зависеть от GUI.

AI не должен зависеть от Slint.

Timeline engine не должен зависеть от конкретного AI provider.

---

# 32. ВАЖНО: НЕ ПЕРЕУСЛОЖНЯТЬ

Не создавать десятки abstraction layers "на будущее", если сейчас они ничего не дают.

Использовать abstractions там, где действительно существует:

- несколько реализаций;
- external dependency;
- необходимость тестирования;
- вероятная замена технологии.

Главная цель:

**работающий продукт, а не архитектурный памятник.**

---

# 33. ТВОЙ РЕЖИМ РАБОТЫ

Не проси меня принимать каждое мелкое техническое решение.

Работай автономно.

Если решение:

- обратимо;
- техническое;
- не меняет продуктовую концепцию;

выбирай самостоятельно лучший вариант.

Останавливайся для вопроса только если решение:

- существенно меняет архитектуру;
- создаёт vendor lock-in;
- требует платной инфраструктуры;
- имеет серьёзные security/privacy последствия;
- противоречит требованиям этого документа.

---

# 34. ПЕРЕД НАПИСАНИЕМ КОДА

Сначала:

1. Изучи актуальное состояние необходимых Rust crates и библиотек.
2. Проверь их maintenance/status/licensing.
3. Исследуй Slint.
4. Исследуй FFmpeg integration.
5. Исследуй wgpu.
6. Исследуй Windows audio/video playback requirements.
7. Исследуй существующие open-source Rust NLE/video projects, если они могут дать полезные архитектурные идеи.
8. Не копируй архитектуру слепо.

После исследования создай:

ARCHITECTURE.md

с:

- выбранным stack;
- diagram;
- crate boundaries;
- media pipeline;
- rendering pipeline;
- threading model;
- project model;
- timeline model;
- AI architecture;
- cache architecture;
- основными рисками.

Затем начинай реализацию.

---

# 35. НЕ ОСТАНАВЛИВАТЬСЯ НА ПЛАНЕ

После ARCHITECTURE.md сразу переходи к реализации.

Не заканчивай работу сообщением:

"Вот план, теперь можно приступать."

Приступай.

Создай repository/workspace.

Собери приложение.

Запускай cargo check/test.

Исправляй compiler errors.

Запускай приложение.

Проверяй UI.

Исправляй runtime errors.

Продолжай до работающего V0.1 настолько далеко, насколько позволяет текущая сессия.

---

# 36. DEVELOPMENT LOOP

Работай итеративно:

Research

→ Architecture

→ minimal vertical slice

→ compile

→ run

→ inspect

→ test

→ fix

→ next feature.

Не пиши сразу 30 000 строк непроверенного кода.

После каждого существенного этапа приложение должно снова собираться.

---

# 37. ПЕРВЫЙ VERTICAL SLICE

Первой целью после scaffolding является:

**Import MP4 → показать его в Media Library → положить на Timeline → воспроизвести Preview → Split → удалить кусок → Export → получить корректный новый MP4.**

Пока этот путь не работает end-to-end, не уходи глубоко в AI.

После этого:

waveform → thumbnails → project persistence → undo/redo → local analysis → AI.

---

# 38. UI QUALITY

Не считать UI завершённым только потому, что кнопки работают.

После появления основного интерфейса проведи отдельный UI/UX pass.

Проверить:

- hierarchy;
- spacing;
- typography;
- icon consistency;
- contrast;
- panel proportions;
- timeline density;
- empty states;
- hover states;
- disabled states;
- progress states;
- loading states;
- error states.

Редактор должен производить впечатление серьёзного профессионального продукта уже при первом запуске.

---

# 39. ОСНОВНАЯ ПРОДУКТОВАЯ ИДЕЯ

Мы не создаём:

"чат, который умеет запускать FFmpeg".

Мы создаём:

**полноценный профессиональный non-linear video editor, в котором AI является естественным дополнительным способом монтажа.**

Человек может работать:

мышью;

клавиатурой;

timeline;

transcript;

чатом;

в будущем голосом.

Все эти способы управляют одним и тем же Project/Timeline/Edit Engine.

---

# 40. КРИТЕРИЙ УСПЕХА

Продукт успешен не тогда, когда AI способен продемонстрировать эффектный demo.

Он успешен, когда реальный монтажёр может загрузить большой концерт, получить первоначальную автоматическую обработку, исправить решения AI вручную и закончить реальный проект существенно быстрее, чем в традиционном редакторе.

AI должен экономить время профессионала, а не отнимать у него контроль.

Начинай с исследования текущего состояния технологий и ARCHITECTURE.md, после чего сразу переходи к реализации первого vertical slice.
