import AppKit
import Foundation

private let otherLabel = "Other"
private let optionShortcutKeys = Array("123456789abcdefghijklmnopqrstuvwxyz")

struct PopupInputRequest: Decodable {
    let questions: [PopupQuestion]
}

struct PopupQuestion: Decodable {
    let id: String
    let question: String
    let options: [PopupOption]
}

struct PopupOption: Decodable {
    let label: String
    let description: String
}

struct PopupInputResponse: Encodable {
    let answers: [String: PopupAnswerValue]

    static func cancelled() -> Self {
        Self(answers: [:])
    }
}

struct PopupAnswerValue: Encodable {
    let answers: [String]
}

private enum AnswerInputSource {
    case keyboard
    case mouse
}

final class RoundedContainerView: NSView {
    var fillColor: NSColor = .controlBackgroundColor {
        didSet { updateAppearance() }
    }

    var strokeColor: NSColor = .separatorColor.withAlphaComponent(0.45) {
        didSet { updateAppearance() }
    }

    var cornerRadius: CGFloat = 12 {
        didSet { updateAppearance() }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        translatesAutoresizingMaskIntoConstraints = false
        wantsLayer = true
        updateAppearance()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    private func updateAppearance() {
        layer?.cornerRadius = cornerRadius
        layer?.backgroundColor = fillColor.cgColor
        layer?.borderColor = strokeColor.cgColor
        layer?.borderWidth = 1
    }
}

final class PopupWindow: NSWindow {
    var onShortcutKey: ((Character) -> Bool)?

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let disallowedModifiers = NSEvent.ModifierFlags([.command, .control, .option, .function])
        guard
            let onShortcutKey,
            event.type == .keyDown,
            event.modifierFlags.intersection(disallowedModifiers).isEmpty,
            let character = event.charactersIgnoringModifiers?.lowercased(),
            character.count == 1,
            let shortcut = character.first
        else {
            return super.performKeyEquivalent(with: event)
        }

        if onShortcutKey(shortcut) {
            return true
        }

        return super.performKeyEquivalent(with: event)
    }
}

final class AutoSelectingTextField: NSTextField {
    var onMouseDown: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        isEditable = true
        isSelectable = true
        isBordered = true
        isBezeled = true
        drawsBackground = true
    }

    convenience init(string: String) {
        self.init(frame: .zero)
        stringValue = string
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override func mouseDown(with event: NSEvent) {
        onMouseDown?()
        super.mouseDown(with: event)
    }
}

final class FlippedView: NSView {
    override var isFlipped: Bool {
        true
    }
}

final class QuestionView: NSStackView, NSTextFieldDelegate {
    private let question: PopupQuestion
    private let shortcuts: [Character?]
    private var optionButtons: [NSButton] = []
    private var optionRows: [RoundedContainerView] = []
    private let customField = AutoSelectingTextField(string: "")
    private var selectedIndex: Int?
    private var answerInputSource: AnswerInputSource?
    private var pendingKeyboardOtherConfirmation = false
    var onAnswerStateChanged: (() -> Void)?
    var onKeyboardAnswerConfirmed: (() -> Void)?

    init(question: PopupQuestion, shortcuts: [Character?]) {
        self.question = question
        self.shortcuts = shortcuts
        super.init(frame: .zero)
        orientation = .vertical
        alignment = .leading
        spacing = 8
        translatesAutoresizingMaskIntoConstraints = false

        let promptLabel = NSTextField(wrappingLabelWithString: question.question)
        promptLabel.font = .systemFont(ofSize: 15, weight: .semibold)
        promptLabel.maximumNumberOfLines = 0
        promptLabel.translatesAutoresizingMaskIntoConstraints = false

        customField.placeholderString = "Type your answer"
        customField.delegate = self
        customField.font = .systemFont(ofSize: 13)
        customField.controlSize = .regular
        customField.onMouseDown = { [weak self] in
            self?.selectOtherOption()
        }
        customField.translatesAutoresizingMaskIntoConstraints = false

        let sectionCard = RoundedContainerView()
        sectionCard.fillColor = .controlBackgroundColor.withAlphaComponent(0.72)
        sectionCard.strokeColor = .separatorColor.withAlphaComponent(0.55)
        sectionCard.cornerRadius = 14

        let options = optionsView()
        sectionCard.addSubview(options)

        addArrangedSubview(promptLabel)
        addArrangedSubview(sectionCard)

        NSLayoutConstraint.activate([
            promptLabel.widthAnchor.constraint(equalTo: widthAnchor),
            sectionCard.widthAnchor.constraint(equalTo: widthAnchor),
            options.topAnchor.constraint(equalTo: sectionCard.topAnchor, constant: 12),
            options.leadingAnchor.constraint(equalTo: sectionCard.leadingAnchor, constant: 12),
            options.trailingAnchor.constraint(equalTo: sectionCard.trailingAnchor, constant: -12),
            options.bottomAnchor.constraint(equalTo: sectionCard.bottomAnchor, constant: -12),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    var selectedAnswer: String? {
        guard let selectedIndex else {
            return nil
        }

        let option = question.options[selectedIndex]
        if isOther(option) {
            let answer = customField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            return answer.isEmpty ? nil : answer
        }

        return option.label
    }

    var hasKeyboardConfirmedAnswer: Bool {
        answerInputSource == .keyboard && selectedAnswer != nil
    }

    func focusFirstInvalidControl() {
        guard let selectedIndex else {
            window?.makeFirstResponder(optionButtons.first)
            return
        }

        if isOther(question.options[selectedIndex]) {
            window?.makeFirstResponder(customField)
        }
    }

    func activateShortcut(at index: Int) {
        if isOther(question.options[index]) {
            beginKeyboardOtherSelection(at: index)
            return
        }

        selectOption(at: index, inputSource: .keyboard)
        onKeyboardAnswerConfirmed?()
    }

    func isEditingCustomField() -> Bool {
        guard let editor = customField.currentEditor() else {
            return false
        }
        return window?.firstResponder === editor
    }

    @objc
    private func selectionChanged(_ sender: NSButton) {
        guard let index = optionButtons.firstIndex(of: sender) else {
            return
        }

        selectOption(at: index, inputSource: .mouse)
    }

    private func selectOption(at index: Int, inputSource: AnswerInputSource?) {
        selectedIndex = index
        answerInputSource = inputSource
        pendingKeyboardOtherConfirmation = false
        refreshSelectionUI()
        onAnswerStateChanged?()

        guard let window else {
            return
        }

        if isOther(question.options[index]), inputSource == .mouse {
            window.makeFirstResponder(customField)
        } else {
            window.endEditing(for: nil)
        }
    }

    func controlTextDidBeginEditing(_ obj: Notification) {
        guard let otherIndex = otherOptionIndex else {
            return
        }

        selectedIndex = otherIndex
        if !pendingKeyboardOtherConfirmation {
            answerInputSource = .mouse
        }
        refreshSelectionUI()
        onAnswerStateChanged?()
        _ = obj
    }

    func controlTextDidChange(_ obj: Notification) {
        guard let otherIndex = otherOptionIndex else {
            return
        }

        selectedIndex = otherIndex
        if !pendingKeyboardOtherConfirmation {
            answerInputSource = .mouse
        }
        refreshSelectionUI()
        onAnswerStateChanged?()
        _ = obj
    }

    func control(
        _ control: NSControl,
        textView: NSTextView,
        doCommandBy commandSelector: Selector
    ) -> Bool {
        guard
            control === customField,
            commandSelector == #selector(NSResponder.insertNewline(_:))
        else {
            return false
        }

        confirmCustomFieldSelection()
        _ = textView
        return true
    }

    private var otherOptionIndex: Int? {
        question.options.firstIndex(where: isOther)
    }

    private func selectOtherOption() {
        guard let otherIndex = otherOptionIndex else {
            return
        }

        selectOption(at: otherIndex, inputSource: .mouse)
    }

    private func beginKeyboardOtherSelection(at index: Int) {
        selectedIndex = index
        answerInputSource = nil
        pendingKeyboardOtherConfirmation = true
        refreshSelectionUI()
        onAnswerStateChanged?()
        window?.makeFirstResponder(customField)
    }

    private func confirmCustomFieldSelection() {
        if pendingKeyboardOtherConfirmation, selectedAnswer != nil {
            answerInputSource = .keyboard
        }

        pendingKeyboardOtherConfirmation = false
        onAnswerStateChanged?()
        window?.endEditing(for: nil)
        window?.makeFirstResponder(nil)

        if answerInputSource == .keyboard, selectedAnswer != nil {
            onKeyboardAnswerConfirmed?()
        }
    }

    private func refreshSelectionUI() {
        for (buttonIndex, button) in optionButtons.enumerated() {
            let isSelected = buttonIndex == selectedIndex
            button.state = isSelected ? .on : .off

            let row = optionRows[buttonIndex]
            row.fillColor = isSelected
                ? .selectedContentBackgroundColor.withAlphaComponent(0.16)
                : .clear
            row.strokeColor = .clear
        }

        customField.alphaValue = selectedIndex.flatMap { isOther(question.options[$0]) ? 1.0 : 0.76 } ?? 0.76
    }

    private func optionsView() -> NSView {
        let optionsStack = NSStackView()
        optionsStack.orientation = .vertical
        optionsStack.alignment = .leading
        optionsStack.spacing = 4
        optionsStack.translatesAutoresizingMaskIntoConstraints = false

        for (index, option) in question.options.enumerated() {
            let button = NSButton(radioButtonWithTitle: "", target: self, action: #selector(selectionChanged))
            button.translatesAutoresizingMaskIntoConstraints = false
            button.setContentHuggingPriority(.required, for: .horizontal)
            optionButtons.append(button)

            let rowContainer = RoundedContainerView()
            rowContainer.fillColor = .clear
            rowContainer.strokeColor = .clear
            rowContainer.cornerRadius = 10
            rowContainer.identifier = NSUserInterfaceItemIdentifier(rawValue: "\(index)")
            optionRows.append(rowContainer)

            let gesture = NSClickGestureRecognizer(target: self, action: #selector(descriptionClicked(_:)))
            rowContainer.addGestureRecognizer(gesture)

            let rowContent: NSView
            if isOther(option) {
                rowContent = otherOptionContent(for: option, button: button, shortcut: shortcuts[index])
            } else {
                rowContent = optionRowContent(
                    description: option.description,
                    button: button,
                    shortcut: shortcuts[index]
                )
            }

            rowContainer.addSubview(rowContent)
            optionsStack.addArrangedSubview(rowContainer)

            NSLayoutConstraint.activate([
                rowContent.topAnchor.constraint(equalTo: rowContainer.topAnchor, constant: 10),
                rowContent.leadingAnchor.constraint(equalTo: rowContainer.leadingAnchor, constant: 10),
                rowContent.trailingAnchor.constraint(equalTo: rowContainer.trailingAnchor, constant: -10),
                rowContent.bottomAnchor.constraint(equalTo: rowContainer.bottomAnchor, constant: -10),
                rowContainer.widthAnchor.constraint(equalTo: optionsStack.widthAnchor),
            ])
        }

        refreshSelectionUI()
        return optionsStack
    }

    private func optionRowContent(
        description: String,
        button: NSButton,
        shortcut: Character?
    ) -> NSView {
        let rowStack = NSStackView()
        rowStack.orientation = .horizontal
        rowStack.alignment = .top
        rowStack.spacing = 10
        rowStack.translatesAutoresizingMaskIntoConstraints = false

        let descriptionLabel = NSTextField(wrappingLabelWithString: description)
        descriptionLabel.maximumNumberOfLines = 0
        descriptionLabel.textColor = .labelColor
        descriptionLabel.font = .systemFont(ofSize: 13)
        descriptionLabel.isSelectable = false
        descriptionLabel.allowsEditingTextAttributes = false
        descriptionLabel.refusesFirstResponder = true

        rowStack.addArrangedSubview(button)
        rowStack.addArrangedSubview(descriptionLabel)
        rowStack.addArrangedSubview(flexibleSpacer())

        if let badge = shortcutBadge(for: shortcut) {
            rowStack.addArrangedSubview(badge)
        }

        return rowStack
    }

    private func otherOptionContent(
        for option: PopupOption,
        button: NSButton,
        shortcut: Character?
    ) -> NSView {
        let rowStack = NSStackView()
        rowStack.orientation = .horizontal
        rowStack.alignment = .centerY
        rowStack.spacing = 10
        rowStack.translatesAutoresizingMaskIntoConstraints = false

        customField.placeholderString = option.description
        customField.setContentHuggingPriority(.defaultLow, for: .horizontal)
        customField.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let fieldContainer = NSView()
        fieldContainer.translatesAutoresizingMaskIntoConstraints = false
        fieldContainer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        fieldContainer.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        fieldContainer.addSubview(customField)

        rowStack.addArrangedSubview(button)
        rowStack.addArrangedSubview(fieldContainer)
        rowStack.addArrangedSubview(flexibleSpacer())

        if let badge = shortcutBadge(for: shortcut) {
            rowStack.addArrangedSubview(badge)
        }

        NSLayoutConstraint.activate([
            customField.heightAnchor.constraint(equalToConstant: 28),
            customField.topAnchor.constraint(equalTo: fieldContainer.topAnchor),
            customField.leadingAnchor.constraint(equalTo: fieldContainer.leadingAnchor),
            customField.trailingAnchor.constraint(equalTo: fieldContainer.trailingAnchor),
            customField.bottomAnchor.constraint(equalTo: fieldContainer.bottomAnchor),
            fieldContainer.widthAnchor.constraint(greaterThanOrEqualToConstant: 220),
        ])

        return rowStack
    }

    @objc
    private func descriptionClicked(_ sender: NSClickGestureRecognizer) {
        guard
            let rawValue = sender.view?.identifier?.rawValue,
            let index = Int(rawValue)
        else {
            return
        }

        selectOption(at: index, inputSource: .mouse)
    }

    private func isOther(_ option: PopupOption) -> Bool {
        option.label.trimmingCharacters(in: .whitespacesAndNewlines)
            .caseInsensitiveCompare(otherLabel) == .orderedSame
    }

    private func shortcutBadge(for shortcut: Character?) -> NSView? {
        guard let shortcut else {
            return nil
        }

        let badge = RoundedContainerView()
        badge.fillColor = .quaternaryLabelColor.withAlphaComponent(0.08)
        badge.strokeColor = .separatorColor.withAlphaComponent(0.22)
        badge.cornerRadius = 7
        badge.setContentHuggingPriority(.required, for: .horizontal)
        badge.setContentCompressionResistancePriority(.required, for: .horizontal)

        let label = NSTextField(labelWithString: String(shortcut).uppercased())
        label.font = .monospacedSystemFont(ofSize: 11, weight: .medium)
        label.textColor = .secondaryLabelColor
        label.translatesAutoresizingMaskIntoConstraints = false

        badge.addSubview(label)
        NSLayoutConstraint.activate([
            label.topAnchor.constraint(equalTo: badge.topAnchor, constant: 3),
            label.leadingAnchor.constraint(equalTo: badge.leadingAnchor, constant: 7),
            label.trailingAnchor.constraint(equalTo: badge.trailingAnchor, constant: -7),
            label.bottomAnchor.constraint(equalTo: badge.bottomAnchor, constant: -3),
        ])

        return badge
    }

    private func flexibleSpacer() -> NSView {
        let spacer = NSView()
        spacer.translatesAutoresizingMaskIntoConstraints = false
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        spacer.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return spacer
    }
}

final class PopupWindowController: NSWindowController, NSWindowDelegate {
    private static let windowWidth: CGFloat = 620
    private static let maxWindowHeight: CGFloat = 800
    private static let topInset: CGFloat = 18
    private static let sideInset: CGFloat = 20
    private static let bottomInset: CGFloat = 18
    private static let contentToActionsSpacing: CGFloat = 14

    private let request: PopupInputRequest
    private let questionViews: [QuestionView]
    private let shortcutAssignments: [[Character?]]
    private let errorLabel = NSTextField(wrappingLabelWithString: "")
    private weak var contentScrollView: NSScrollView?
    private weak var contentStack: NSStackView?
    private weak var actionsView: NSView?
    private var contentScrollHeightConstraint: NSLayoutConstraint?
    private var isClosingProgrammatically = false
    private(set) var response = PopupInputResponse.cancelled()

    init(request: PopupInputRequest) {
        self.request = request
        self.shortcutAssignments = Self.assignShortcuts(for: request)
        self.questionViews = zip(request.questions, shortcutAssignments).map { question, shortcuts in
            QuestionView(question: question, shortcuts: shortcuts)
        }

        let window = PopupWindow(
            contentRect: NSRect(x: 0, y: 0, width: Self.windowWidth, height: 420),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "Request User Input"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self
        window.onShortcutKey = { [weak self] shortcut in
            self?.handleShortcutKey(shortcut) ?? false
        }
        wireQuestionCallbacks()
        buildInterface()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func present() -> PopupInputResponse {
        guard let window else {
            return .cancelled()
        }

        NSApp.activate(ignoringOtherApps: true)
        showWindow(nil)
        updateWindowSizing()
        let targetFrame = presentedFrame(for: window)
        let startFrame = hiddenStartFrame(for: targetFrame, window: window)
        window.setFrame(startFrame, display: false)
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(nil)

        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.22
            context.allowsImplicitAnimation = true
            window.animator().setFrame(targetFrame, display: true)
        }

        _ = NSApp.runModal(for: window)
        return response
    }

    func windowWillClose(_ notification: Notification) {
        guard let window else {
            return
        }

        if isClosingProgrammatically {
            _ = notification
            return
        }

        response = .cancelled()
        if NSApp.modalWindow === window {
            NSApp.stopModal(withCode: .abort)
        }

        _ = notification
    }

    @objc
    private func submit(_ sender: Any?) {
        guard let firstInvalid = questionViews.first(where: { $0.selectedAnswer == nil }) else {
            let answers = Dictionary(uniqueKeysWithValues: zip(request.questions, questionViews).compactMap { pair in
                let (question, view) = pair
                return view.selectedAnswer.map { answer in
                    (question.id, PopupAnswerValue(answers: [answer]))
                }
            })
            setValidationMessage(nil)
            response = PopupInputResponse(answers: answers)
            close(with: .OK)
            return
        }

        setValidationMessage("Choose one answer for every question.")
        firstInvalid.focusFirstInvalidControl()
        _ = sender
    }

    @objc
    private func cancel(_ sender: Any?) {
        response = .cancelled()
        close(with: .abort)
        _ = sender
    }

    private func close(with code: NSApplication.ModalResponse) {
        guard let window else {
            return
        }

        isClosingProgrammatically = true
        let finalFrame = window.frame.offsetBy(dx: 0, dy: -12)

        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.16
            context.allowsImplicitAnimation = true
            window.animator().alphaValue = 0
            window.animator().setFrame(finalFrame, display: true)
        } completionHandler: {
            NSApp.stopModal(withCode: code)
            window.orderOut(nil)
            window.alphaValue = 1
            window.close()
        }
    }

    private func wireQuestionCallbacks() {
        for view in questionViews {
            view.onAnswerStateChanged = { [weak self] in
                self?.setValidationMessage(nil)
            }
            view.onKeyboardAnswerConfirmed = { [weak self] in
                self?.setValidationMessage(nil)
                self?.submitIfAllAnswersWereConfirmedByKeyboard()
            }
        }
    }

    private func handleShortcutKey(_ shortcut: Character) -> Bool {
        guard !questionViews.contains(where: { $0.isEditingCustomField() }) else {
            return false
        }

        for (questionIndex, shortcuts) in shortcutAssignments.enumerated() {
            guard let optionIndex = shortcuts.firstIndex(where: { $0 == shortcut }) else {
                continue
            }

            questionViews[questionIndex].activateShortcut(at: optionIndex)
            return true
        }

        return false
    }

    private func submitIfAllAnswersWereConfirmedByKeyboard() {
        guard
            questionViews.allSatisfy({ $0.selectedAnswer != nil }),
            questionViews.allSatisfy(\.hasKeyboardConfirmedAnswer)
        else {
            return
        }

        submit(nil)
    }

    private func buildInterface() {
        guard let contentView = window?.contentView else {
            return
        }

        let backgroundView = NSVisualEffectView()
        backgroundView.translatesAutoresizingMaskIntoConstraints = false
        backgroundView.material = .windowBackground
        backgroundView.blendingMode = .behindWindow
        backgroundView.state = .active

        let contentStack = NSStackView()
        contentStack.orientation = .vertical
        contentStack.alignment = .leading
        contentStack.spacing = 14
        contentStack.translatesAutoresizingMaskIntoConstraints = false
        contentStack.detachesHiddenViews = true

        errorLabel.textColor = .systemRed
        errorLabel.font = .systemFont(ofSize: 12)
        errorLabel.maximumNumberOfLines = 0
        errorLabel.stringValue = ""
        errorLabel.isHidden = true

        let header = headerView()
        header.setContentHuggingPriority(.required, for: .vertical)
        header.setContentCompressionResistancePriority(.required, for: .vertical)
        contentStack.addArrangedSubview(header)
        contentStack.setCustomSpacing(16, after: header)

        for view in questionViews {
            view.setContentHuggingPriority(.required, for: .vertical)
            view.setContentCompressionResistancePriority(.required, for: .vertical)
            contentStack.addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: contentStack.widthAnchor).isActive = true
        }

        errorLabel.setContentHuggingPriority(.required, for: .vertical)
        errorLabel.setContentCompressionResistancePriority(.required, for: .vertical)
        contentStack.addArrangedSubview(errorLabel)
        let actions = buttonRow()
        actions.setContentHuggingPriority(.required, for: .vertical)
        actions.setContentCompressionResistancePriority(.required, for: .vertical)

        let scrollView = NSScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = false
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true

        let documentView = FlippedView()
        documentView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.documentView = documentView
        documentView.addSubview(contentStack)

        contentView.addSubview(backgroundView)
        backgroundView.addSubview(scrollView)
        backgroundView.addSubview(actions)

        let scrollHeightConstraint = scrollView.heightAnchor.constraint(equalToConstant: 200)
        self.contentScrollHeightConstraint = scrollHeightConstraint
        self.contentScrollView = scrollView
        self.contentStack = contentStack
        self.actionsView = actions

        NSLayoutConstraint.activate([
            backgroundView.topAnchor.constraint(equalTo: contentView.topAnchor),
            backgroundView.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            backgroundView.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            backgroundView.bottomAnchor.constraint(equalTo: contentView.bottomAnchor),
            scrollView.topAnchor.constraint(equalTo: backgroundView.topAnchor, constant: Self.topInset),
            scrollView.leadingAnchor.constraint(equalTo: backgroundView.leadingAnchor, constant: Self.sideInset),
            scrollView.trailingAnchor.constraint(equalTo: backgroundView.trailingAnchor, constant: -Self.sideInset),
            actions.leadingAnchor.constraint(equalTo: backgroundView.leadingAnchor, constant: Self.sideInset),
            actions.trailingAnchor.constraint(equalTo: backgroundView.trailingAnchor, constant: -Self.sideInset),
            actions.bottomAnchor.constraint(equalTo: backgroundView.bottomAnchor, constant: -Self.bottomInset),
            actions.topAnchor.constraint(greaterThanOrEqualTo: scrollView.bottomAnchor, constant: Self.contentToActionsSpacing),
            scrollHeightConstraint,
            contentStack.topAnchor.constraint(equalTo: documentView.topAnchor),
            contentStack.leadingAnchor.constraint(equalTo: documentView.leadingAnchor),
            contentStack.trailingAnchor.constraint(equalTo: documentView.trailingAnchor),
            contentStack.bottomAnchor.constraint(equalTo: documentView.bottomAnchor),
            contentStack.widthAnchor.constraint(equalTo: scrollView.contentView.widthAnchor),
        ])

        updateWindowSizing()
    }

    private func headerView() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 3
        stack.translatesAutoresizingMaskIntoConstraints = false

        let titleLabel = NSTextField(labelWithString: "Choose an answer for each question")
        titleLabel.font = .systemFont(ofSize: 18, weight: .semibold)

        let subtitleLabel = NSTextField(
            wrappingLabelWithString:
                "Choose one option in each section. Shortcuts 1-9 and A-Z work when a custom field is not focused."
        )
        subtitleLabel.font = .systemFont(ofSize: 12)
        subtitleLabel.textColor = .secondaryLabelColor
        subtitleLabel.maximumNumberOfLines = 0
        subtitleLabel.translatesAutoresizingMaskIntoConstraints = false

        stack.addArrangedSubview(titleLabel)
        stack.addArrangedSubview(subtitleLabel)
        subtitleLabel.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true

        return stack
    }

    private func buttonRow() -> NSView {
        let buttons = NSStackView()
        buttons.orientation = .horizontal
        buttons.spacing = 12
        buttons.alignment = .centerY
        buttons.translatesAutoresizingMaskIntoConstraints = false

        let spacer = NSView()
        spacer.translatesAutoresizingMaskIntoConstraints = false
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        spacer.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let cancelButton = NSButton(title: "Cancel", target: self, action: #selector(cancel))
        let submitButton = NSButton(title: "Submit", target: self, action: #selector(submit))
        cancelButton.keyEquivalent = "\u{1b}"
        submitButton.keyEquivalent = "\r"

        buttons.addArrangedSubview(spacer)
        buttons.addArrangedSubview(cancelButton)
        buttons.addArrangedSubview(submitButton)

        return buttons
    }

    private func setValidationMessage(_ message: String?) {
        let value = message ?? ""
        errorLabel.stringValue = value
        errorLabel.isHidden = value.isEmpty
        updateWindowSizing()
    }

    private func updateWindowSizing() {
        guard
            let window,
            let contentView = window.contentView,
            let contentStack,
            let contentScrollView,
            let actionsView,
            let contentScrollHeightConstraint
        else {
            return
        }

        contentView.layoutSubtreeIfNeeded()
        let contentHeight = contentStack.fittingSize.height
        let actionsHeight = actionsView.fittingSize.height
        let chromeHeight = window.frameRect(forContentRect: NSRect(x: 0, y: 0, width: Self.windowWidth, height: 0)).height
        let maxContentRectHeight = max(0, Self.maxWindowHeight - chromeHeight)
        let fixedHeight = Self.topInset + Self.contentToActionsSpacing + actionsHeight + Self.bottomInset
        let availableScrollableHeight = max(0, maxContentRectHeight - fixedHeight)
        let targetScrollHeight = min(contentHeight, availableScrollableHeight)
        let targetContentRectHeight = fixedHeight + targetScrollHeight

        contentScrollHeightConstraint.constant = targetScrollHeight
        contentScrollView.hasVerticalScroller = contentHeight > availableScrollableHeight
        contentScrollView.verticalScrollElasticity = contentHeight > availableScrollableHeight ? .automatic : .none

        contentView.layoutSubtreeIfNeeded()
        window.setContentSize(NSSize(width: Self.windowWidth, height: targetContentRectHeight))
    }

    private func presentedFrame(for window: NSWindow) -> NSRect {
        let visibleFrame = activeVisibleFrame(for: window)
        let size = window.frame.size
        let originX = visibleFrame.midX - (size.width / 2)
        let originY = visibleFrame.minY + 16
        return NSRect(origin: NSPoint(x: originX, y: originY), size: size)
    }

    private func hiddenStartFrame(for targetFrame: NSRect, window: NSWindow) -> NSRect {
        let visibleFrame = activeVisibleFrame(for: window)
        var frame = targetFrame
        frame.origin.y = visibleFrame.minY - frame.height
        return frame
    }

    private func activeVisibleFrame(for window: NSWindow?) -> NSRect {
        let mouseLocation = NSEvent.mouseLocation
        if let screen = NSScreen.screens.first(where: { $0.frame.contains(mouseLocation) }) {
            return screen.visibleFrame
        }
        if let screen = window?.screen {
            return screen.visibleFrame
        }
        if let screen = NSScreen.main {
            return screen.visibleFrame
        }
        let fallbackHeight = max(window?.frame.height ?? 420, 420) + 32
        let fallbackWidth = max(window?.frame.width ?? Self.windowWidth, Self.windowWidth)
        return NSRect(x: 0, y: 0, width: fallbackWidth, height: fallbackHeight)
    }

    private static func assignShortcuts(for request: PopupInputRequest) -> [[Character?]] {
        var cursor = optionShortcutKeys.startIndex
        return request.questions.map { question in
            question.options.map { _ in
                guard cursor < optionShortcutKeys.endIndex else {
                    return nil
                }

                defer { cursor = optionShortcutKeys.index(after: cursor) }
                return optionShortcutKeys[cursor]
            }
        }
    }

    private static func isOtherLabel(_ label: String) -> Bool {
        label.trimmingCharacters(in: .whitespacesAndNewlines)
            .caseInsensitiveCompare(otherLabel) == .orderedSame
    }
}

enum PopupInputError: Error {
    case invalidRequest(String)
    case invalidResponse(String)
}

extension PopupInputError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case let .invalidRequest(message), let .invalidResponse(message):
            return message
        }
    }
}

private func decodeRequest(from data: Data) throws -> PopupInputRequest {
    guard !data.isEmpty else {
        throw PopupInputError.invalidRequest("missing popup input request JSON on stdin")
    }

    do {
        return try JSONDecoder().decode(PopupInputRequest.self, from: data)
    } catch {
        throw PopupInputError.invalidRequest("failed to decode popup input request JSON: \(error)")
    }
}

private func showPopup(for request: PopupInputRequest) throws -> PopupInputResponse {
    guard !request.questions.isEmpty else {
        return .cancelled()
    }

    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    app.finishLaunching()

    let controller = PopupWindowController(request: request)
    return controller.present()
}

private func writeResponse(_ response: PopupInputResponse) throws {
    do {
        let data = try JSONEncoder().encode(response)
        FileHandle.standardOutput.write(data)
    } catch {
        throw PopupInputError.invalidResponse("failed to encode popup input response JSON: \(error)")
    }
}

do {
    let request = try decodeRequest(from: FileHandle.standardInput.readDataToEndOfFile())
    let response = try showPopup(for: request)
    try writeResponse(response)
} catch {
    fputs("\(error.localizedDescription)\n", stderr)
    exit(1)
}
