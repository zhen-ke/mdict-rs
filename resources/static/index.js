// =============================================
// MDict 极速词典 - 增强版
// =============================================

// 配置
const CONFIG = {
    HISTORY_MAX: 20,           // 最大历史记录数
    DEBOUNCE_MS: 200,          // 搜索防抖延迟
    HISTORY_KEY: 'mdx_history' // localStorage 键名
};

// 全局状态
let suggestTimer = null;
let selectedIndex = -1;
let currentQuery = '';  // 当前查询词，用于高亮
let isNavigating = false; // 防止重复导航

// =============================================
// URL 路由功能
// =============================================

/**
 * 从 URL hash 获取查询词
 * 支持格式: #/word/dictionary 或 #dictionary
 */
function getWordFromUrl() {
    const hash = window.location.hash;
    if (!hash) return null;

    // 格式: #/word/dictionary
    const match = hash.match(/^#\/word\/(.+)$/);
    if (match) {
        return decodeURIComponent(match[1]);
    }

    // 简单格式: #dictionary
    if (hash.length > 1 && !hash.startsWith('#/')) {
        return decodeURIComponent(hash.slice(1));
    }

    return null;
}

/**
 * 更新 URL hash（不触发 hashchange 事件的查询）
 */
function updateUrl(word, addToHistory = true) {
    if (!word) {
        if (window.location.hash) {
            history.pushState(null, '', window.location.pathname);
        }
        return;
    }

    const newHash = '#/word/' + encodeURIComponent(word);

    if (addToHistory) {
        // 添加到浏览器历史
        history.pushState({ word: word }, '', newHash);
    } else {
        // 替换当前状态（不添加历史记录）
        history.replaceState({ word: word }, '', newHash);
    }

    // 更新页面标题
    document.title = word + ' - MDict 极速词典';
}

/**
 * 处理浏览器后退/前进
 */
function handlePopState(event) {
    const word = getWordFromUrl();

    if (word) {
        isNavigating = true;
        $('#word').val(word);
        queryMdxFromNavigation(word);
    } else {
        // 回到首页状态
        $('#word').val('');
        showWelcome();
        document.title = 'MDict 极速词典';
    }
}

/**
 * 从导航触发的查询（不更新 URL）
 */
function queryMdxFromNavigation(word) {
    if (!word || !validInput(word)) {
        return;
    }

    currentQuery = word;
    showLoading();
    hideHistoryDropdown();
    hideSuggestions();

    $.ajax({
        url: './query',
        type: 'POST',
        data: { 'word': word },
        dataType: 'html',
        success: function (data) {
            isNavigating = false;
            if (data && data.trim() !== '' && !data.includes('not found')) {
                let highlighted = highlightSearchTerm(data, currentQuery);
                $('#mdx-resp').html(highlighted).show();
                $('#share-btn').show();
                document.title = word + ' - MDict 极速词典';
            } else {
                showEmpty();
            }
        },
        error: function(xhr) {
            isNavigating = false;
            if (xhr.status === 404) {
                showEmpty();
            } else {
                showError('查询出错，请稍后重试');
            }
        }
    });
}

/**
 * 显示欢迎页面
 */
function showWelcome() {
    $('#mdx-resp').html(`
        <div class="empty-state">
            <div class="empty-state-icon">🔍</div>
            <p>请在上方输入单词开始查询</p>
        </div>
    `);
}

// 监听浏览器后退/前进
window.addEventListener('popstate', handlePopState);

// =============================================
// 初始化
// =============================================
$(document).ready(function () {
    initHistoryDropdown();

    // 检查 URL 是否有查询词
    const wordFromUrl = getWordFromUrl();
    if (wordFromUrl) {
        $('#word').val(wordFromUrl);
        queryMdxFromNavigation(wordFromUrl);
    } else {
        $('#word').focus();
    }
});

// =============================================
// 查询历史功能 (localStorage)
// =============================================
function getHistory() {
    try {
        return JSON.parse(localStorage.getItem(CONFIG.HISTORY_KEY)) || [];
    } catch (e) {
        return [];
    }
}

function saveHistory(word) {
    if (!word || word.length < 2) return;

    let history = getHistory();
    // 移除重复项
    history = history.filter(w => w.toLowerCase() !== word.toLowerCase());
    // 添加到开头
    history.unshift(word);
    // 限制数量
    if (history.length > CONFIG.HISTORY_MAX) {
        history = history.slice(0, CONFIG.HISTORY_MAX);
    }
    localStorage.setItem(CONFIG.HISTORY_KEY, JSON.stringify(history));
}

function clearHistory() {
    localStorage.removeItem(CONFIG.HISTORY_KEY);
    hideHistoryDropdown();
}

function initHistoryDropdown() {
    // 创建历史记录下拉框
    if ($('#history-dropdown').length === 0) {
        $('#search-wrapper').append(`
            <div id="history-dropdown" class="history-dropdown" style="display: none;">
                <div class="history-header">
                    <span>🕒 查询历史</span>
                    <button id="clear-history" class="clear-history-btn">清空</button>
                </div>
                <ul id="history-list"></ul>
            </div>
        `);
    }
}

function showHistoryDropdown() {
    const history = getHistory();
    if (history.length === 0) return;

    const $list = $('#history-list');
    $list.empty();

    history.forEach((word, index) => {
        $list.append(`<li data-word="${word}">${word}</li>`);
    });

    $('#history-dropdown').show();
    $('#suggestions').hide();
}

function hideHistoryDropdown() {
    $('#history-dropdown').hide();
}

// 输入框获得焦点且为空时显示历史
$(document).on('focus', '#word', function() {
    if ($(this).val().trim() === '') {
        showHistoryDropdown();
    }
});

// 点击历史项
$(document).on('click', '#history-list li', function() {
    const word = $(this).data('word');
    $('#word').val(word);
    hideHistoryDropdown();
    queryMdx(word);
});

// 清空历史
$(document).on('click', '#clear-history', function(e) {
    e.stopPropagation();
    clearHistory();
});

// =============================================
// Loading 动画
// =============================================
function showLoading() {
    $('#mdx-resp').html(`
        <div class="loading-state">
            <div class="loading-spinner"></div>
            <p>查询中...</p>
        </div>
    `);
}

function showError(message) {
    $('#mdx-resp').html(`
        <div class="empty-state error-state">
            <div class="empty-state-icon">❌</div>
            <p>${message}</p>
        </div>
    `);
}

function showEmpty() {
    $('#share-btn').hide();
    $('#mdx-resp').html(`
        <div class="empty-state">
            <div class="empty-state-icon">🔍</div>
            <p>未找到相关词条</p>
        </div>
    `);
}

// =============================================
// 搜索高亮 & 一键复制
// =============================================
function highlightSearchTerm(html, term) {
    if (!term || term.length < 2) return html;

    // 创建临时 DOM 解析
    const $temp = $('<div>').html(html);

    // 只在 .def, .defcn, .example 等文本节点中高亮，不影响 HTML 结构
    const textNodes = [];
    const walker = document.createTreeWalker(
        $temp[0],
        NodeFilter.SHOW_TEXT,
        null,
        false
    );

    while (walker.nextNode()) {
        textNodes.push(walker.currentNode);
    }

    const regex = new RegExp(`(${escapeRegExp(term)})`, 'gi');

    textNodes.forEach(node => {
        if (node.nodeValue.match(regex)) {
            const span = document.createElement('span');
            span.innerHTML = node.nodeValue.replace(regex, '<mark class="search-highlight">$1</mark>');
            node.parentNode.replaceChild(span, node);
        }
    });

    return $temp.html();
}

function escapeRegExp(string) {
    return string.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

// 分享链接功能
$(document).on('click', '#share-btn', function(e) {
    e.preventDefault();
    const url = window.location.href;

    navigator.clipboard.writeText(url).then(() => {
        const $btn = $(this);
        const originalText = $btn.html();
        $btn.html('✅ 已复制');
        setTimeout(() => $btn.html(originalText), 2000);
    }).catch(err => {
        console.error('复制链接失败:', err);
        // 降级方案：显示链接让用户手动复制
        prompt('复制此链接分享:', url);
    });
});

// =============================================
// 核心查询功能
// =============================================
function queryMdx(word, updateHistory = true) {
    if (!word || !validInput(word)) {
        return;
    }

    currentQuery = word;
    showLoading();
    hideHistoryDropdown();
    hideSuggestions();

    // 更新 URL（用户主动查询时）
    if (!isNavigating) {
        updateUrl(word, updateHistory);
    }

    $.ajax({
        url: './query',
        type: 'POST',
        data: { 'word': word },
        dataType: 'html',
        success: function (data) {
            if (data && data.trim() !== '' && !data.includes('not found')) {
                // 保存到历史
                saveHistory(word);

                // 高亮搜索词
                let highlighted = highlightSearchTerm(data, currentQuery);

                $('#mdx-resp').html(highlighted).show();

                // 添加复制按钮

                // 显示分享按钮
                $('#share-btn').show();

                // 更新页面标题
                document.title = word + ' - MDict 极速词典';
            } else {
                showEmpty();
            }
        },
        error: function(xhr) {
            if (xhr.status === 404) {
                showEmpty();
            } else {
                showError('查询出错，请稍后重试');
            }
        }
    });
}

function postQuery() {
    let word = $('#word').val().trim();
    if (!validInput(word)) {
        return;
    }
    queryMdx(word);
}

// 验证输入
function validInput(word) {
    return word
        && word.length > 0
        && word !== '.'
        && word !== '#'
        && word !== '?'
        && word !== '/';
}

// =============================================
// 联想词功能
// =============================================
function showSuggestions(suggestions) {
    let $list = $('#suggestions');
    $list.empty();
    hideHistoryDropdown();

    if (suggestions.length === 0) {
        $list.hide();
        return;
    }

    suggestions.forEach((word, index) => {
        // 高亮匹配部分
        const query = $('#word').val().trim().toLowerCase();
        let displayWord = word;
        if (word.toLowerCase().startsWith(query)) {
            displayWord = `<strong>${word.substring(0, query.length)}</strong>${word.substring(query.length)}`;
        }
        $list.append($('<li>').html(displayWord).data('word', word).data('index', index));
    });

    selectedIndex = -1;
    $list.show();
}

function hideSuggestions() {
    $('#suggestions').hide();
    selectedIndex = -1;
}

function selectSuggestion(index) {
    let $items = $('#suggestions li');
    $items.removeClass('selected');

    if (index >= 0 && index < $items.length) {
        $items.eq(index).addClass('selected');
        selectedIndex = index;
    } else {
        selectedIndex = -1;
    }
}

// 监听输入变化
$(document).on('input', '#word', function() {
    let query = $(this).val().trim();
    hideHistoryDropdown();

    if (suggestTimer) {
        clearTimeout(suggestTimer);
    }

    if (query.length < 2) {
        hideSuggestions();
        if (query.length === 0) {
            showHistoryDropdown();
        }
        return;
    }

    // 防抖
    suggestTimer = setTimeout(function() {
        $.ajax({
            url: './suggest',
            type: 'GET',
            data: { q: query },
            dataType: 'json',
            success: function(data) {
                showSuggestions(data);
            },
            error: function() {
                hideSuggestions();
            }
        });
    }, CONFIG.DEBOUNCE_MS);
});

// 点击建议项
$(document).on('click', '#suggestions li', function() {
    let word = $(this).data('word');
    $('#word').val(word);
    hideSuggestions();
    queryMdx(word);
});

// 点击其他地方隐藏下拉框
$(document).on('click', function(e) {
    if (!$(e.target).closest('#search-wrapper').length) {
        hideSuggestions();
        hideHistoryDropdown();
    }
});

// =============================================
// 键盘事件
// =============================================
$(document).on('keydown', '#word', function(e) {
    let $items = $('#suggestions li');
    let isVisible = $('#suggestions').is(':visible');

    if (e.keyCode === 13) { // Enter
        e.preventDefault();
        hideHistoryDropdown();
        if (isVisible && selectedIndex >= 0) {
            let word = $items.eq(selectedIndex).data('word');
            $('#word').val(word);
            hideSuggestions();
            queryMdx(word);
        } else {
            hideSuggestions();
            postQuery();
        }
    } else if (e.keyCode === 40 && isVisible) { // Down
        e.preventDefault();
        selectSuggestion(Math.min(selectedIndex + 1, $items.length - 1));
    } else if (e.keyCode === 38 && isVisible) { // Up
        e.preventDefault();
        selectSuggestion(Math.max(selectedIndex - 1, 0));
    } else if (e.keyCode === 27) { // Escape
        hideSuggestions();
        hideHistoryDropdown();
    }
});

// =============================================
// 链接点击处理
// =============================================
$(document).on('click', 'a', function (e) {
    let href = $(this).attr('href');

    // 处理 sound:// 协议的音频播放
    if (href && href.startsWith('sound://')) {
        e.preventDefault();
        let audioPath = href.replace('sound://', '/');

        let audio = document.getElementById('mdict-audio');
        if (!audio) {
            audio = document.createElement('audio');
            audio.id = 'mdict-audio';
            document.body.appendChild(audio);
        }
        audio.src = audioPath;
        audio.play().catch(err => console.error('Audio play failed:', err));
        return;
    }

    // 词典内部链接
    if (href && href.startsWith('/') && !href.startsWith('/#')) {
        e.preventDefault();
        let word = href.slice(1);
        $('#word').val(word);
        queryMdx(word);
    }
});

// =============================================
// 快捷键
// =============================================
$(window).bind('keyup keydown', function (e) {
    if ((e.ctrlKey || e.metaKey) && String.fromCharCode(e.which).toLowerCase() === 'l') {
        e.preventDefault();
        $('#word').val('').focus();
        scrollTo(0, 0);
        showHistoryDropdown();
    }
});

// =============================================
// 查询按钮（修复：空输入不查询）
// =============================================
$(document).on('click', '#lucky-btn', function (e) {
    let word = $('#word').val().trim();

    // 如果输入框有内容，执行查询
    if (word && validInput(word)) {
        queryMdx(word);
    } else {
        // 输入框为空时，执行"试试手气"
        $.ajax({
            url: './lucky',
            type: 'GET',
            dataType: 'html',
            success: function (data) {
                if (data && data.trim() !== '') {
                    $('#mdx-resp').html(data).show();
                } else {
                    showEmpty();
                }
            },
            error: function() {
                showError('获取随机词条失败');
            }
        });
    }
});
