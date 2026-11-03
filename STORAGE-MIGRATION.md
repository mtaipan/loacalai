# Индексированное хранение чатов: проект следующего блока

Дата: 3 ноября 2026. Основание: read-only разбор HEAD `fe5b990` и текущих точек вызова. Это проект реализации, а не описание уже выполненной миграции. Работающий сервер, пользовательские чаты и функциональный код в рамках этого разбора не менялись.

## Что уже сделано и что остаётся

`19b582c` уже сохраняет JSON потоком через `PreparedReplacement::write`, без промежуточного `Vec<u8>` всего файла. Owned storage guard остаётся в blocking worker при отмене async caller. Не нужно заново «чинить атомарный save» возвратом к `to_vec` или ранним освобождением блокировки.

`771154f` уже отделяет браузерную проекцию от `history`/`material`: `chat_pages::page` декодирует только окно `VisibleMessage`. Но `chat_headers::read` сначала проходит весь JSON для счётчика, затем `chat_pages::page` ещё раз проходит весь JSON с `IgnoredAny`. CPU/IO остаются линейными по размеру чата; одно гигантское выбранное `content` всё ещё становится целой строкой.

Главная нагрузка активного запроса осталась: `engine::run` загружает весь `Conversation`, хранит `messages`/`history` в RAM, трижды сохраняет целый чат и при несовместимом model-history восстанавливает контекст из всех видимых сообщений до применения бюджета. Перевод одного поля `save()` на SQLite этого не исправит.

## Решение и граница минимального связного блока

Сделать SQLite источником истины для **метаданных чата, видимой ленты, попыток ответа, извлечённого материала и модельной рабочей истории**. Использовать существующий `tasks.sqlite3`, чтобы принятие Run, привязка Task и новый пользовательский turn публиковались одной транзакцией. Не вводить вторую БД чатов с независимым commit от `runs`.

Перевести текущий engine на операции над текущим turn и ограниченным модельным контекстом. Убрать `App::load/save(Conversation)` из рабочих путей, rename и обработчика ошибки. Старый `Conversation` сохранить как compatibility DTO для тестов/формата экспорта, не как runtime store.

Связный блок включает импорт старых файлов, engine adapter, browser paging, отдельное чтение большого сообщения, серверный потоковый экспорт, retry/deletion/crash recovery и проверки. Индекс плюс прежний full-load engine не считается завершением этого блока. Семантический поиск, полная перестройка UI и изменение формата пользовательских документов сюда не входят.

## Предлагаемая схема

Новая версия schema после текущей v10 — через общий `Store::open`, с проверкой неизвестной новой версии и сохранением исходной БД. Точное имя версии определить на момент реализации, не предполагать отсутствие параллельных migrations.

| Таблица | Основные поля / ограничения | Назначение |
|---|---|---|
| `conversations` | `id PK`, title, mode, updated, `timeline_revision`, `metadata_revision`, `next_position`, `active_count`, state | Индекс sidebar и атомарная версия ленты. ID сверяется с legacy filename. Удалённые ID не переиспользуются |
| `chat_turns` | `(conversation, position) PK`, current_attempt, user_message, legacy flag | Стабильная позиция пользовательского раунда; retry не сдвигает старые позиции |
| `chat_attempts` | `run PK`, conversation, position, attempt_number, state, request fingerprint, supersedes, created | Принятая попытка, идемпотентность, retry и восстановление; run связан с существующим Run |
| `chat_messages` | internal PK, conversation, position, ordinal, attempt, original_message_id, role, created, content_body, material_body, envelope_body | Видимые user/assistant. Уникальность нового сообщения по `(run, role/ordinal)`; legacy message IDs могут повторяться, поэтому они не PK |
| `chat_text_bodies` / `chat_text_chunks` | body PK, bytes, Unicode scalar count, SHA256, completed; `(body, chunk_no) PK`, byte/character offsets | Неизменяемые тексты и JSON envelopes порциями, например до 16 КиБ UTF-8. Большое сообщение не равно одной SQLite TEXT строке |
| `chat_history_generations` | id PK, conversation, run, model, workspace, task, status, policy_version | Отдельная модельная история; UI её не получает |
| `chat_history_rounds` | `(generation, round_no) PK`, first_item, last_item, estimated_tokens, state | Полные user→assistant/tool*→assistant раунды и проверенная граница tool batch |
| `chat_history_items` | `(generation, item_no) PK`, round_no, role, body, estimated_tokens | Точный сохранённый рабочий payload после private-input projection, отдельно от видимых messages |
| `chat_imports` | conversation/source hash PK, original_path, archive_path, byte_count, state, next checkpoint, validation report | Возобновляемый staging импорта, происхождение и сохранение исходных байтов |

Индексы: conversations по `(state,updated,id)`; chat_turns по conversation/position; messages по `(conversation,position,ordinal,attempt)`; attempts по conversation/run и уникальному request/run; history по generation/round/item. Внешние ключи и индексы для deletion должны быть явными, не полагаться на JSON fields или timestamp-only sorting.

**Не переиспользовать `artifact_bodies` без изменения их GC.** Текущий GC считает body живым по `artifacts`/`material_ingests`; новая ссылка сообщения в другой таблице иначе будет удалена при удалении чужого чата. Для первого блока безопаснее отдельные `chat_text_*`. Полные tool receipts по-прежнему находятся в существующем Run/artifact journal; envelope сообщения содержит ссылки и совместимое окно карточек. Исторические legacy tools без Run сохраняются отдельно целиком, не теряются при windowing.

Bodies сначала пишутся в staging короткими транзакциями, затем запечатываются после hash/count проверки. Сообщение ссылается только на completed body. GC staging проверяет operation lease/status и не удаляет тело живого импорта/запроса. Нельзя держать общую write transaction на время inference, полного экспорта или копирования многогигабайтного legacy файла.

## Контракт запросов и retry tail

Новые методы store:

1. `accept_turn(input, accepted_settings, run_id)` — внутри текущей транзакции принятия Run проверить tombstone, fingerprint повторного request ID, активный Run, expected workspace и expected tail revision. Создать attempt/user message и повысить revision. Вернуть небольшие `TurnHandle`/`ChatHeader`, а не весь чат. Принятие HTTP и принятие данных — одно durable событие.
2. `set_turn_material(handle, completed_body, source_refs)` — сверить attempt/run/state; привязать извлечение к конкретному user message. Не использовать «последнее сообщение», которое могло измениться.
3. `history_for_turn(handle, model, workspace, task, budget)` — получить совместимую generation или fallback из последних **полных** видимых раундов, без загрузки всей ленты.
4. `checkpoint_history(handle, complete_batches, pending_batch)` — сохранить то, что действительно получено и выполнено. Pending call без всех ответов имеет явный state; после crash не становится completed round.
5. `finish_turn(handle, answer_body, envelope, history_generation, result)` — условный идемпотентный commit по run/attempt, append assistant, обновление counts/revision. `finish_error` использует ту же уникальность: ошибка wrapper не создаёт второго assistant после уже опубликованного финала.

Retry — новая attempt **того же turn**, с новым Run. Предыдущий answer, извлечение и tool receipts сохраняются как superseded. UI показывает активную attempt; экспорт истории попыток возможен отдельно. Новая attempt должна содержать точный принятый user content и attachment IDs, включая изменённые параметры новой попытки, а не молча брать «последний user».

Переключение видимой attempt и создание Run атомарны. Если ответ retry завершился ошибкой, показывается именно failed новая попытка с доступом к прежней; не объявлять старый успешный ответ результатом новой попытки. Выбор такого поведения соответствует текущей замене tail, но теперь без физического уничтожения старых данных.

Идемпотентность request ID не заменяет сравнение payload: тот же ID с другим conversation/content/attachments/mode/task/accepted scope — conflict. Повтор одинакового принятого запроса возвращает тот же attempt/run, без append. Не вычислять fingerprint из усечённого preview. Порядок ленты — монотонные позиции, не UUID/timestamp.

## Модельная история и память

История для inference не равна полному архиву сообщений. Перед чтением тел рассчитать бюджет через существующие `context::plan`, tools/system/current-input cost и resource admission. Существующие per-request transport bounds и Task ledger content поля этим блоком автоматически не снимаются; `runs.input`/`task_requests.content` и их readers требуют отдельной согласованной миграции на refs при снятии лимита одного принятого запроса. Bounded chat archive не следует объявлять доказательством bounded памяти всех Task endpoints. Оценки токенов сохранять вместе с version estimator; при смене алгоритма перевычислять выбранные записи или консервативно учитывать старую оценку.

Читать метаданные раундов с конца индексом и набирать только помещающиеся полные раунды. Затем декодировать их тела. Не делать `SELECT all items` → `Vec<Value>` → `fit_history` для многолетнего чата. Текущий запрос и обязательные инструкции всегда защищены; если они сами не помещаются, сохранить исходник и вернуть точное состояние, а не незаметно отрезать запрос.

Tool call и все его tool replies — неделимый пакет. Во время активного turn существующий `fit_history` может уменьшать tool payload через immutable artifact refs и complete-batch checkpoint; в durable generation сохранять и границы, и основание сокращения. Не брать только последние N сообщений: это может начать историю с сиротского tool reply.

Fallback при model/workspace/task mismatch должен повторять нынешние правила `same_workspace/same_task`, но читать только подходящие последние раунды. Извлечённые `material` и источники читать по body/source refs в пределах выбранного контекста. Исходные пользовательские требования из Task ledger остаются доступными через `read_task`; новый chat store не создаёт второй «истинный план задачи».

В случае гигантского старого раунда сначала читать counts/token estimates; если раунд не помещается, включить доступное резюме/ссылку как явно производный checkpoint или оставить его в архиве с признаком неполноты контекста. Не декодировать сотни мегабайт ради вывода «не помещается». Смысловая полнота от наличия checkpoint не считается доказанной.

## Импорт JSON и сохранность исходных данных

Ленивый импорт под conversation operation lease при первом обращении; пользовательские другие чаты продолжают работать. Первичное перечисление файлов/sidebar не запускает импорт всех bodies. Для ещё не импортированных чатов допустим существующий streamed header fallback с явной отдельной ценой IO, но не считать его окончательной indexed реализацией.

1. Зафиксировать источник под conversation operation lease; глобальный storage mutex не держать на протяжении копирования всех байтов. Потоком скопировать исходный файл в `data/chat-originals/<id>/<sha256>.json` через существующий `PreparedReplacement::write`, одновременно считать hash/size. Повторно проверить исходный descriptor/revision перед публикацией. После публикации архива `sync_all` и durable import intent; исходный `chats/<id>.json` не удалять и не переписывать.
2. Импортировать **архив**, а не изменяемый исходный путь, в staging rows/checkpoints. Полностью проверить JSON, expected ID, message count, surrogate/Unicode и закрытие контейнеров. Только потом одна короткая transaction публикует imported conversation. Никакая половина чата не становится видимой как весь чат.
3. Подтвердить counts, body hashes и сверку значимых полей; записать report/authority marker в SQLite. До marker JSON остаётся authority; после marker SQLite — authority. JSON после этого является зафиксированным original, не вторым live store. Dual write не вводить.
4. После crash staging продолжает импорт immutable archive с checkpoint. До публикации ошибка возвращает исходный JSON через прежний путь либо явный recovery state; после публикации **нельзя** молча откатываться к устаревшему JSON при ошибке SQLite — это потеряет новые сообщения.

Сохранить оригинальный JSON **байт-в-байт**: форматирование, escape spelling, неизвестные поля и metadata остаются в original archive. Нормализованный compatibility export сохраняет значения, но не обещает те же байты/порядок ключей. Некорректный, неоднозначный или неподдерживаемый JSON не «чинится» автоматически; исходник сохраняется, импорт не публикуется, ошибка локальна конкретному чату. Не перезаписывать повреждённый файл пустым новым чатом.

Важная отдельная реализационная задача: `serde_json::from_reader::<Message>` всё равно выделит целую строку одного гигантского `content`. Для bounded import недостаточно message-by-message visitor. Нужен строгий streaming token reader `json_stream` с container stack, incremental string-unescape в chunk sink и проверкой surrogate pairs, либо проверенная библиотека с таким контрактом. Сканер должен сохранять raw spans и незнакомые values в source archive; числовые/служебные поля отдельно валидируются. Не писать собственный общий JSON formatter. Перед включением importer нужны fixture/fuzz тесты строк/экранирований/границ буфера/ошибок; если этот слой не готов, не заявлять снятие per-message RAM проблемы.

Архив по умолчанию хранится до явного удаления чата. Удаление создаёт durable cleanup intent, закрывает доступ tombstone и затем удаляет только app-owned `chat-originals/<id>` и старый `chats/<id>.json`; внешние пользовательские файлы и отдельно созданные резервные копии не удаляются. Crash cleanup возобновляется по проверяемому ID/собственным путям, без произвольных путей из содержимого JSON. Tombstone остаётся даже после удаления архива, чтобы старый файл не воскресил чат.

Legacy message ID может быть повторным/произвольным: собственный internal key + сохранённое original ID. Не генерировать фиктивные successful Run/tool receipts из старых assistant текстов. Существующий `legacy::import_legacy_requests` заменить reader-адаптером user rows с теми же matching/signature/Task-boundary правилами; не импортировать уже journaled запросы повторно.

Изменение старого JSON внешней программой после authority marker — конфликт источника, а не сигнал перезатереть SQLite. Показать возможность отдельного импорта версии; исходник и новые SQLite turns сохраняются оба. При downgrade старое приложение не знает marker; поэтому рабочий data home нельзя объявлять совместимым со старым binary. Для возврата — явный экспорт/восстановление в **другую** папку, без автоматического downgrade.

## Браузер, большие сообщения и экспорт

`/api/chats/{id}/messages` сохраняет нынешнюю форму page metadata и глобальные индексы в compatibility adapter, но выбирает данные SQL индексом. Cursor включает conversation/generation или `timeline_revision` и стабильную позицию. Append допускает snapshot чтение прежнего диапазона; retry/delete меняют структурную revision и дают conflict/refresh, а не смешивают attempts. Rename не должен инвалидировать body pagination: отдельная metadata revision.

Число сообщений страницы **не ограничивает** размер одного сообщения. Каждое visible message получает bounded `content` preview + `content_partial`, body descriptor, characters/bytes, source revision. Полный текст: `/api/chats/{id}/messages/{internal_id}/content?offset=&max_characters=&revision=`; проверка принадлежности чату, tombstone и точной body revision на каждом диапазоне. Offset — Unicode scalar count, не JS UTF-16 index; UI не вычисляет серверный cursor длиной JS строки.

Большие `diagnostics`/legacy tool arrays/attachments тоже требуют отдельного диапазона/списка, а не единственного большого JSON envelope в message page. Рабочие private `history` и `material` никогда не подмешиваются в обычный browser response. Для инструмента `read_message` сначала достаточно scoped ссылки через уже имеющийся Task ledger; не расширять доступ к чужим чатам под видом body ID.

Copy/edit/save используют полный body endpoint; не копируют preview как полный текст. Для очень большого clipboard/edit browser всё равно может потребовать весь текст — это реальная особенность действия, UI должен показывать прогресс/отмену и не собирать его при обычном открытии чата. Markdown renderer работает с показанным диапазоном и явно отмечает его; не парсить произвольную середину fence как целое сообщение без контекста.

Серверный `/api/chats/{id}/export?format=markdown|json&attempts=active|all` потоково выдаёт все требуемые данные, без `GET full chat` → JS `map/join` → Blob. Экспорт Markdown содержит видимое содержание/вложения/реальные сохранённые tool данные; нормализованный JSON compatibility export может содержать private рабочую историю **только** как отдельный явно выбранный полный резервный экспорт, не как browser view. Original archive скачивается отдельно и точно соответствует сохранённым байтам.

Экспорт получает логический snapshot: immutable bodies и выбранные row IDs/attempt generation. Не держать SQL read transaction/WAL snapshot на часы медленного download. Короткой transaction создать export manifest с references, которое удерживает bodies от GC; отдавать manifest страницами и освободить lease по завершении/отмене. При deletion либо экспорт прерывается с закрытием доступа, либо ранее явно созданный backup остаётся по описанной политике — решение должно быть единым. Для минимального блока: прерывать живой download при tombstone, не продолжать выдавать удалённые данные.

## Точные точки замены

Номера строк относятся к просмотренному состоянию `fe5b990`; соседние незакоммиченные document edits могут их сдвинуть.

| Точка | Замена |
|---|---|
| `api.rs` `Message/Conversation` около196–234 | Compatibility DTO остаются; добавить `TurnHandle`, `MessageDescriptor`, `HistorySelection`, не передавать Conversation в engine |
| `api.rs` `App::load/load_locked/header/load_view/save` около360–402 | Indexed repository; full load только deprecated explicit compatibility reader, исключённый из рабочих callers |
| `api.rs` `chats/new_chat/chat/chat_messages/rename` около850–913 | SQL metadata/keyset list, empty create, window projection, metadata-only rename, streamed compatibility GET если он нужен тестам/внешнему клиенту |
| `api.rs` `start_run` около1200–1280 | Run admission + user attempt в одной transaction, возврат прежнего Run при duplicate; legacy request import через repository iterator |
| `engine.rs` около868–908 | Вместо load/pop/push/save получить уже принятый TurnHandle и header; удаление tail заменить retry-attempt relation |
| `engine.rs` около957–960 | `set_turn_material` по message/attempt, без перезаписи архива |
| `engine.rs` около1075–1083 | `history_for_turn` по budget и complete rounds вместо clone/full fallback `message_context` всех messages |
| `engine.rs` около1140 | Browser context hint из scoped journal/index по текущей Task/attempt; не загружать старые сообщения ради `.rev().take(12)` |
| `engine.rs` около1308–1318 | Durable history generation/checkpoint вместо присваивания поля chat.history; текущий vector остаётся bounded inference payload |
| `engine.rs` около1338 | Citation replacement меняет current answer body/history item того же attempt, не «последний элемент любого чата» |
| `engine.rs` около1348–1351 | `finish_turn`, короткие saved metadata/counts, событие после commit |
| `api.rs` error wrapper около1331–1350 | Идемпотентный `finish_error(run)`; убрать load/last-role heuristic/save всего чата |
| `api.rs` deletion около925 + `task_store::delete_conversation` | Tombstone и chat rows в одной transaction с Run/Task deletion; dependent stores cleanup по уже известным IDs, recovery intent сохраняется |
| `legacy.rs::import_legacy_requests` | Итерация сохранённых user records, тот же dedup/provenance/Task rules; память не растёт со всей Conversation |
| `chat_pages.rs`, `chat_headers.rs` | Сохранить legacy import/fallback tests, runtime paging/list перевести на SQL. Не возвращать private fields |
| `web/session.js` около169–170 | Optimistic tail связан stable turn/attempt IDs и accepted revision, не pop по локальному последнему role/index |
| `web/app.js` `exportChat` около277, copy/save/edit около397–399 | Server download и full-body fetch для явного действия; partial marker и range navigation |
| `changes.rs::PreparedReplacement` | Сохранить как есть для original archive/export файлов; не использовать JSON rewrite как второй live store |

## Порядок реализации и проверки публикации

1. Добавить schema/body chunks/import staging/repository и unit tests. Старый runtime ещё authority, до включения marker новые tables не меняют поведение.
2. Добавить bounded strict importer и сверку на изолированном корпусе; проверить crash до/после archive commit и authority transaction, unknown fields, corrupt/truncated JSON, huge single string. Legacy files остаются неизменными.
3. Перевести admission/engine/finish-error/retry целиком на repository. Это одна согласованная смена callers: нельзя публиковать mixed путь, в котором новый finish записывает SQL, а старый fallback save затирает JSON/SQLite.
4. Подключить indexed browser projection/full-body/export, rename/delete/sidebar. Compat full GET выдавать потоком из snapshot, новый фронт его не вызывает для экспорта/обычной навигации.
5. Включать authority marker per conversation после migration validation. Удалить production callers full Conversation store. Исходные архивы и явный recovery route сохраняются.

Обязательные проверки на собственных fixtures:

- 100k сообщений: открыть последние24 и добавить turn с RSS/временем, зависящими от выбранного окна/нового turn, а не от всего архива; отдельно измерить одноразовую цену migration.
- Одна строка сообщения/diagnostics/material на128+МиБ, CRLF, NUL как JSON escape, emoji/surrogates, UTF-8 на границах chunks: побайтовый original archive и точный range/export без full decode на обычном page.
- Crash/fault injection после каждого durable boundary: body staging, archive publication, import activation, acceptance, extraction, tool batch, finish before saved event. Дубликат request/finalization не создаёт второй turn/answer.
- Cancel async caller во время blocking commit сохраняет текущую гарантию `19b582c`; Stop остаётся отзывчивым, export/import не держат общий writer lock на весь файл.
- Retry completed/failed/cancelled turn с attachments, после перезапуска, с другим проектом/model/task: старая attempt сохранена; snapshot не смешивает обе; effects не проигрываются автоматически.
- Tool batch: несколько calls, часть replies, malformed/private input, crash, модельная compaction. Никаких orphan replies и возврата скрытого input через diagnostics/history/export view.
- Append во время чтения старых страниц; rename; retry; deletion; late finish после tombstone; другой клиент и stale expected tail. Метаданные/sidebar не требуют чтения всех bodies.
- Удаление чата не удаляет shared bodies другого чата, не затрагивает project files/attachments без отдельной политики; original archive/recovery retention описан явно. Существующий tombstone не позволяет импортировать удалённый старый JSON заново.
- Ошибка диска/SQLite corruption/unknown schema: данные сохранены, один повреждённый чат не исчезает молча и не превращается в пустой. Export cancellation не оставляет вечную GC lease; зависший download не удерживает бесконечный WAL read snapshot.
- Совместимость всех режимов Chat/Agent/Team/Search/Translate, Browser reload/background chats, request-id reconnect, existing legacy import matching, permission scopes и Task continuity. Все интеграции — собственные данные, fake inference, без перезапуска пользовательского сервера.

Граница доказательства: этот документ основан на статическом чтении текущего кода. Новые schema/importer/endpoints не реализованы и не проверены; оценка RSS/скорости, физические GPU/macOS и качество модельных резюме здесь не измерялись. Проверка миграции обязана отдельно подтвердить сохранность и поведение; один успешный SQL test не доказывает готовность всего блока.
