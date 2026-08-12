# 验证快速记录与主页的交互原型

## Problem Statement

QuickNote 的核心心智模型横跨两个 surface：用户在快速记录浮窗中以最少打断继续追加当前便签，又在主页中查看、切换和整理便签。如果这两个 surface 对“当前便签”、新建、自动保存、归档、时间和语音状态的反馈不一致，用户会不确定内容写到了哪里、是否已经保存，以及下一次快捷键会打开哪一张便签。

在进入生产实现前，需要用低保真可交互原型验证操作可发现性、焦点与键盘路径、控件密度、响应式布局和简约现代的视觉层级，同时避免把尚未决定的桌面技术栈、存储、通知和语音架构提前固化。

## Solution

使用一个明确标记为 throwaway 的单页交互原型，以共享内存状态模拟快速记录浮窗和主页。原型提供三个结构明显不同、可通过 `?variant=A|B|C` 分享和切换的 UI variants：

- `A — 专注接力`：宽窗口在右侧直接编辑，单栏由快速记录浮窗承接；使用紧凑品牌栏，并将归档降级为低频次要入口。
- `B — 命令中心`：紧凑导航、表格式列表和操作带，突出键盘效率与信息密度。
- `C — 空间画布`：当前便签 hero 和便签画布，突出内容层级与操作可发现性。

三个 variants 共用相同的用户旅程和状态语义。原型不连接真实持久化、Windows 全局快捷键、系统通知或语音转写服务；刷新页面即重置状态。

原型验证表明，快速记录浮窗与主页可以围绕同一个“当前便签”模型形成连贯旅程。建议生产设计以 `A — 专注接力` 作为基础：它最直接地展示主页便签集合与当前便签的关系；吸收 `B` 的紧凑元数据和键盘提示，以及 `C` 的强当前便签视觉层级。`B` 的三栏结构在窄窗口下信息跳跃较大，`C` 的画布虽然清晰，但会降低高密度浏览效率，因此不应原样采用。

## User Stories

1. As a QuickNote user, I want the global shortcut to open the current note, so that I can continue recording without first navigating the home page.
2. As a QuickNote user, I want focus to land in the editor when quick capture opens, so that I can type immediately.
3. As a QuickNote user, I want the caret to start at the end of the current note on first open, so that quick capture naturally appends rather than overwrites earlier content.
4. As a QuickNote user, I want my existing caret position to survive visible autosave updates, so that feedback does not interrupt editing.
5. As a keyboard user, I want `Esc` to close quick capture, so that returning to the home page is fast and predictable.
6. As a keyboard user, I want focus to return to the control that opened quick capture, so that I do not lose my place after closing it.
7. As a QuickNote user, I want a visible current-note indicator on the home page, so that I know which note the global shortcut will open.
8. As a QuickNote user, I want opening another active note from the home page to make it current immediately, so that the next capture continues in that direction.
9. As a QuickNote user, I want switching the current note to leave the former note intact, so that changing attention does not modify its lifecycle.
10. As a QuickNote user, I want both the list and editor to reflect a current-note switch, so that the two home-page regions never disagree.
11. As a QuickNote user, I want a clear new-note action on the home page, so that starting a new direction is discoverable.
12. As a QuickNote user, I want a new note to open as a blank current note, so that I can begin recording immediately.
13. As a QuickNote user, I want an untouched blank new note to disappear when I cancel it, so that accidental opens leave no clutter.
14. As a QuickNote user, I want a new note to become durable in the interaction model only after the first non-blank input, so that empty notes are never represented as saved work.
15. As a QuickNote user, I want the former current note to remain unchanged when a new note is created, so that starting something new is reversible and safe.
16. As a QuickNote user, I want visible saving feedback after editing, so that I know the application noticed my input.
17. As a QuickNote user, I want saving feedback to settle automatically without a manual save action, so that capture stays interruption-free.
18. As a QuickNote user, I want automatic saving to work in both quick capture and the home-page editor, so that the surfaces follow one rule.
19. As a QuickNote user, I want the note title to derive from its first non-empty line, so that creating a note does not require a separate naming step.
20. As a QuickNote user, I want an untitled empty draft to be labelled clearly as a blank note, so that its temporary state is understandable.
21. As a QuickNote user, I want to archive the current note from either surface, so that I can stop following it without changing context first.
22. As a QuickNote user, I want archived notes to leave the home page immediately, so that the main view stays focused on active directions.
23. As a QuickNote user, I want future reminders to stop when a note is archived, so that inactive notes do not notify me.
24. As a QuickNote user, I want a short-lived undo action after archiving, so that accidental archive operations are inexpensive to recover from.
25. As a QuickNote user, I want to browse archived notes separately from the home page, so that recovery does not add noise to active work.
26. As a QuickNote user, I want restoring an archived note to return it to the home page, so that I can resume following it.
27. As a QuickNote user, I want a restored note to become current, so that resuming it and quick capture agree immediately.
28. As a QuickNote user, I want a due time to describe the expected completion of the whole note, so that its scope is unambiguous.
29. As a QuickNote user, I want the first due time to default the reminder to the same moment, so that the common case is one action.
30. As a QuickNote user, I want to change or clear a reminder independently, so that notification timing does not redefine the due time.
31. As a QuickNote user, I want due and reminder metadata visible without opening a settings screen, so that time-sensitive notes are recognizable at a glance.
32. As a QuickNote user, I want the time editor to explain the default linkage, so that the result is predictable before I save it.
33. As a QuickNote user, I want voice recording to begin only after an explicit action, so that QuickNote never appears to listen continuously.
34. As a QuickNote user, I want a strong visible recording state, so that I know when audio capture is active.
35. As a QuickNote user, I want the same control to stop recording, so that recording is a simple two-state interaction.
36. As a QuickNote user, I want the transcription inserted at my current caret, so that voice input behaves like another editing method.
37. As a QuickNote user, I want recording to stop when quick capture closes, so that capture cannot continue invisibly.
38. As a keyboard user, I want every primary control to expose a visible focus ring, so that I can track focus without a pointer.
39. As a keyboard user, I want prototype variant arrow shortcuts disabled while editing text, so that comparison controls never steal normal caret navigation.
40. As a narrow-window user, I want the two-column home page to become a single reading column, so that controls and content remain usable without horizontal scrolling.
41. As a narrow-window user, I want quick capture to fit within the viewport, so that writing remains the dominant action.
42. As a QuickNote user, I want active and archived collections to use explicit labels, so that lifecycle state is not communicated by colour alone.
43. As a QuickNote user, I want destructive and restorative actions to use distinct language, so that archiving is not confused with deletion or completion.
44. As a product reviewer, I want all variants to expose the same state transitions, so that visual alternatives can be compared fairly.
45. As a product reviewer, I want each variant encoded in the URL, so that a specific proposal can be shared and revisited.
46. As a product reviewer, I want the relevant in-memory state visible in the prototype, so that every action's effect can be inspected directly.
47. As a developer, I want the prototype isolated from production code and persistence, so that interaction learning does not constrain the final architecture.
48. As a developer, I want real OS integration excluded from this prototype, so that technical feasibility decisions remain with their dedicated issues.
49. As a wide-window user, I want selecting a note to focus the persistent editor without opening another surface, so that the home page remains a direct editing workspace.
50. As a single-column user, I want selecting a note to open quick capture, so that editing remains available when the persistent editor is hidden.
51. As a QuickNote user, I want new-note creation at the lower-right of the note collection, so that the action is visually associated with the collection it changes.
52. As a QuickNote user, I want reminder metadata shown before due metadata, so that the earlier intervention is read before the final deadline.

## Implementation Decisions

- Use one highest-level acceptance seam: drive the complete user journey through the visible quick-capture and home-page surfaces while observing their shared note state.
- Treat `便签`, `当前便签`, `主页`, `截止时间`, `提醒` and `归档` according to the project domain glossary; do not replace them with note, task, inbox, completion or deletion semantics in Chinese product copy.
- Use one state owner for both surfaces. The current note identifier, note collection, temporary draft, active/archived filter, save feedback, recording state and current surface are represented together.
- Opening any active note from the home page changes the current note immediately; the next shortcut activation opens that same note in quick capture.
- On wide layouts, selecting a note changes the current note and focuses the persistent editor without opening quick capture. On single-column layouts where the persistent editor is hidden, the same selection opens quick capture.
- Quick capture is reserved for shortcut activation and layouts without a persistent editor. The wide home-page editor does not repeat a `快速记录` action.
- New-note creation is located at the lower-right of the note collection. It opens an uncommitted draft in the persistent editor on wide layouts and in quick capture on single-column layouts.
- When both reminder and due metadata are present, reminder is displayed before due time.
- The first quick-capture focus goes to the editor with the caret at the content end. Renders caused by autosave feedback preserve the user's current selection.
- New-note creation begins as an uncommitted in-memory draft. The draft joins the note collection and becomes current only after non-blank input; closing an untouched draft discards it without trace.
- Autosave is the only save interaction. The UI exposes transient `正在保存…` and settled `已自动保存` feedback without a save button.
- A note's display title is derived from its first non-empty line; the remaining non-empty lines form its preview.
- Archiving removes a note from the home page and clears its future reminder. A temporary undo restores the pre-archive snapshot.
- Archived notes are browsed in a separate collection. Restoring returns the note to the home page and makes it current.
- The home page does not repeat its identity across brand, page title and collection controls. The active collection is the default context, while archived notes use a visually secondary text entry with an optional count rather than an equal-weight tab.
- Due time and reminder belong to the whole note. When a user adds a due time and no reminder exists, the reminder defaults to the same value; later reminder changes remain independent.
- Voice capture is an explicit two-state action. Ending it inserts transcription at the current caret. Closing quick capture always ends the recording state.
- On widths below the two-column breakpoint, the persistent editor is hidden and quick capture becomes the editing surface, leaving one note-list column without horizontal overflow. The exact production breakpoint remains an implementation detail to tune with the chosen desktop shell.
- The production direction should start from variant A's explicit list/current-note relationship, combine variant B's compact metadata and keyboard guidance, and borrow variant C's strong current-note hierarchy. None of the throwaway variant code should be promoted directly.
- The prototype switcher is not a product control. It updates a URL search parameter, wraps across variants, responds to left/right arrows outside editing controls and remains visually separate from the evaluated UI.
- The final framework, component system, persistence mechanism and desktop window implementation remain open until their dedicated architecture decisions are resolved.

## Testing Decisions

- Good tests assert external behavior across the visible surface boundary rather than CSS classes, internal state object shape, timer implementation or framework components.
- The preferred acceptance seam is a desktop-shell journey test that can invoke the application's quick-capture command, interact with the home page, and observe the resulting current note and visible content. This remains one seam even if the shell internally routes an OS hotkey into the same command.
- Test the current-note journey: quick capture opens the current note, focuses the editor at the end, home-page selection changes current immediately, and the next quick capture opens that selected note.
- Test both responsive selection paths: a wide home page changes and focuses the persistent editor without a floating window, while a single-column home page opens quick capture for the selected note.
- Test that the shortcut always opens quick capture at both wide and narrow widths, and that the wide persistent editor has no duplicate `快速记录` action.
- Test new-note lifecycle: an untouched draft cancels without creating a note; the first non-blank input creates it and makes it current; the former current note is unchanged.
- Test autosave by observing settled visible feedback and re-reading through the application's public note surface after the persistence design exists. Do not test debounce timers or private storage calls directly.
- Test archive behavior through the home page: the note disappears from active notes, future reminder presentation is absent, undo restores it, and archived browsing plus restore returns it as current.
- Test due/reminder behavior through the time editor: first due time defaults reminder, explicit reminder change is retained, and clearing reminder leaves due time intact.
- Test voice interaction with a fake transcription boundary: recording state is visible, stop inserts a deterministic transcript at the caret, and closing the surface stops recording. Real engine accuracy and Windows microphone integration belong to separate tests.
- Test keyboard paths: global-command routing, initial focus, `Esc`, focus restoration, visible focus styling and no interception of editor arrow keys.
- Test responsive behavior at representative wide and narrow desktop viewports, asserting one-column flow and absence of horizontal overflow rather than exact pixel values.
- No prior application tests exist in the repository. This prototype and its documented browser journey are the current prior art; implementation should preserve the high-level seam when the production test harness is chosen.
- The throwaway prototype itself does not receive an automated test suite. It was manually browser-verified for page load, three variants, console errors, core journeys, keyboard behavior and responsive layouts.

## Out of Scope

- Production application code or direct promotion of any prototype component.
- Selection of Tauri, WPF, Slint, WinUI or another desktop framework.
- Real Windows `RegisterHotKey`, single-instance activation, tray, blur-on-click-outside or window-positioning behavior.
- Disk persistence, schema, migration, export and crash-recovery design.
- Real Windows notification scheduling, action routing, missed-reminder recovery and snooze delivery.
- Real microphone permission, audio capture, transcription model/service selection, model download and transcription accuracy.
- Search, Markdown rendering, recycle bin/deletion, settings, startup behavior and application updates.
- Pixel-perfect visual design, motion polish, final typography and final component tokens.
- macOS, Linux, mobile, Web, accounts, sync, collaboration, attachments, tags, folders and plugins.

## Further Notes

- Browser verification exposed two useful interaction defects that were corrected in the prototype: initial quick-capture focus placed the caret at the beginning rather than the append position, and the time dialog initially lacked an accessible name.
- Verified behaviors include non-empty rendering with no console errors, shareable `A/B/C` variants, quick-capture focus, append and autosave feedback, blank draft cancellation, current-note switching, archive/undo, archived restore, due/reminder defaulting and independence, recording/transcription state, keyboard variant switching outside editors and narrow single-column layout without horizontal overflow.
- The prototype is a primary source captured on the dedicated throwaway branch. Its state inspector is evaluation scaffolding and must not appear in production.
