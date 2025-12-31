// 光标默认可输入
$(document).ready(function (e) {
        $('#word').focus();
    }
);

// 查询mdx
function queryMdx(word) {
    $('#mdx-resp').html('查询中...');
    $.ajax({
        url: './query',
        type: 'POST',
        data: {'word': word},
        dataType: 'html',
        success: function (data) {
            if (data !== '') {
                $('#mdx-resp').html(data).show();
            } else {
                $('#mdx-resp').hide();
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

// 特殊字符不查询
function validInput(word) {
    return word
        && word !== '.'
        && word !== '#'
        && word !== '?'
        && word !== '/';
}

// 联想词功能
let suggestTimer = null;
let selectedIndex = -1;

function showSuggestions(suggestions) {
    let $list = $('#suggestions');
    $list.empty();

    if (suggestions.length === 0) {
        $list.hide();
        return;
    }

    suggestions.forEach((word, index) => {
        $list.append($('<li>').text(word).data('index', index));
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

// 监听输入变化（使用事件委托确保 DOM 加载后也能工作）
$(document).on('input', '#word', function() {
    let query = $(this).val().trim();

    if (suggestTimer) {
        clearTimeout(suggestTimer);
    }

    if (query.length < 2) {
        hideSuggestions();
        return;
    }

    // 防抖：200ms 后请求
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
    }, 200);
});

// 点击建议项
$(document).on('click', '#suggestions li', function() {
    let word = $(this).text();
    $('#word').val(word);
    hideSuggestions();
    queryMdx(word);
});

// 点击其他地方隐藏建议
$(document).on('click', function(e) {
    if (!$(e.target).closest('#search-wrapper').length) {
        hideSuggestions();
    }
});

// 监听键盘：回车、上下键（使用事件委托）
$(document).on('keydown', '#word', function(e) {
    let $items = $('#suggestions li');
    let isVisible = $('#suggestions').is(':visible');

    if (e.keyCode === 13) { // Enter
        e.preventDefault();
        if (isVisible && selectedIndex >= 0) {
            let word = $items.eq(selectedIndex).text();
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
    }
});

// 监听牛津8解释页面的外部单词链接
$(document).on('click', 'a', function (e) {
    console.log($(this).attr('href'));
    let href = $(this).attr('href');// '/cool'

    // 处理 sound:// 协议的音频播放
    if (href && href.startsWith('sound://')) {
        e.preventDefault();
        // 将 sound://hwd/bre/8/xxx.mp3 转换为 /hwd/bre/8/xxx.mp3
        let audioPath = href.replace('sound://', '/');
        console.log('Playing audio:', audioPath);

        // 创建或复用 audio 元素
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

    if (href && href.startsWith('/') && !href.startsWith('/#')) {
        $('#word').val(href.slice(1)) // 'cool'
        postQuery();
        e.preventDefault()
    }
});

// 捕获ctrl+L快捷键
$(window).bind('keyup keydown', function (e) {
    if ((e.ctrlKey || e.metaKey)
        && String.fromCharCode(e.which).toLowerCase() === 'l') {
        e.preventDefault();
        $('#word').val('').focus();
        scrollTo(0, 0);
    }
});

// 试试手气按钮
$(document).on('click', '#lucky-btn', function (e) {
    $.ajax({
        url: './lucky',
        type: 'GET',
        dataType: 'html',
        success: function (data) {
            if (data !== '') {
                $('#mdx-resp').html(data).show();
            } else {
                $('#mdx-resp').hide();
            }
            // $('#word').val(parserWordFromResp(data))
        }
    });
});

// 不同词典返回html不一样，无法通用
// function parserWordFromResp(data) {
//     let el = document.createElement('html');
//     el.innerHTML = data;
//     let top_g = el.getElementsByClassName("top-g")[0]
//     if (top_g == null) {
//         console.log("top-g is null");
//         return "";
//     }
//
//     return top_g.firstElementChild.innerHTML.split('·').join('')
//
// }
