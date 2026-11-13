import type { GlyphName } from "../ui/icons";
import { zhCommon } from "./zh/common";
import { zhHost } from "./zh/host";
import { zhInteraction } from "./zh/interaction";
import { zhPlayback } from "./zh/playback";
import { zhClient } from "./zh/client";
import { zhStats } from "./zh/stats";
import { zhViewer } from "./zh/viewer";

// Canonical catalog: add keys here first; every other locale must match it.
// Copy is split by namespace; this object keeps the published shape.
export const zh = {
  ...zhCommon,
  ...zhHost,
  ...zhInteraction,
  ...zhPlayback,
  ...zhClient,
  ...zhStats,
  ...zhViewer,
} as const;

export type CopyKey = keyof typeof zh;

export const zhTitleFrames = {
  hostActive: { label: zh["host.live"], variations: [
    "屏幕已亮",
    "像素出发",
    "画面开跑",
    "小电视上工",
    "好戏开场",
    "有画面，才有真相",
    "窗口营业中",
    "这波有画面",
    "像素在整活",
    "画面已开席",
    "大幕拉开",
    "好康的端上来了",
    "独乐乐不如众乐乐",
    "诸位，请看大屏幕",
    "围观副本已开启",
    "本台正在播出",
    "把快乐打在屏幕上",
    "今日份名场面",
    "承包这一屏快乐",
  ] },
  viewerActive: { label: zh["viewer.title.watching"], variations: [
    "已经就座",
    "像素到站",
    "正在围观",
    "爆米花就位",
    "画面收到",
    "小电视开工",
    "沙发已占好",
    "前方高能",
    "名场面围观中",
    "这就很有画面",
    "小板凳已搬好",
    "路过，坐下，围观",
    "让我康康",
    "有内味儿了",
    "此处应有瓜子",
    "前排出售花生瓜子",
    "我看得大受震撼",
    "围观也是参与",
    "好看，爱看",
  ] },
  paused: { label: zh["host.paused"], variations: [] },
  hostStarting: { label: zh["host.starting"], variations: [
    "开场准备中",
    "正在给画面搭台",
    "小电视准备上工",
    "像素正在集合",
    "好戏还在候场",
    "好康的正在赶来",
    "围观副本准备中",
    "开场前先清清嗓子",
    "灯光师正在就位",
    "画面正在候场",
    "正在安排像素出场",
    "开席前再摆摆盘",
    "舞台正在布置",
    "正在酝酿名场面",
    "小电视热身中",
    "给大屏幕报个幕",
    "开场仪式准备中",
  ] },
  hostEnded: { label: zh["host.ended"], variations: [] },
  hostReady: { label: zh["host.roomReady"], variations: [
    "沙发已摆好",
    "随时可以开场",
    "小电视待命",
    "遥控器就位",
    "灯光已调好",
    "频道等你",
    "茶水已备好",
    "万事俱备，就等画面",
    "客官，里面请",
    "前排虚位以待",
    "本客厅接受围观",
    "是时候秀一手了",
    "道具齐了，主角请",
    "选个画面就开席",
    "压箱底的，好康的",
    "请开始你的分享",
    "沙发有位，故事等你",
    "舞台交给你了",
    "随时开启围观副本",
  ] },
  hostIdle: { label: zh["host.notStarted"], variations: [
    "小电视在休息",
    "等一块新屏幕",
    "天线在打盹",
    "屏幕做个梦",
    "遥控器在发呆",
    "等一个开场",
    "小客厅静悄悄",
    "本台暂时摸鱼",
    "今日份发呆时间",
    "先歇会儿，待会儿整活",
    "今日无事，电视休沐",
    "小电视进入待机",
    "画面尚未营业",
    "屏幕正在养精蓄锐",
    "本电视正在挂机",
    "本台稍后再营业",
    "茶先泡着，画面待定",
    "不慌，先喝口茶",
    "待机，也是一种状态",
  ] },
  viewerReady: { label: "点击播放", variations: [] },
  viewerClosed: { label: zh["viewer.msg.closed"], variations: [] },
  viewerUnavailable: { label: "暂不可用", variations: [] },
  hostUnavailable: { label: zh["host.fail.start"], variations: [] },
  viewerWaiting: { label: zh["viewer.title.waiting"], variations: [
    "等待开场",
    "爆米花准备中",
    "小板凳已搬好",
    "沙发留了位置",
    "遥控器握好了",
    "正在等画面",
    "观众席待命",
    "蹲一个开场",
    "先码住，等画面",
    "前排等更新",
    "瓜子备好了，就等画面",
    "等一波名场面",
    "画面未至，板凳先到",
    "候场区已就座",
    "来都来了，等会儿",
    "等主角登场",
    "接下来，看房主的",
    "小电视还在候场",
    "围观群众等开席",
  ] },
} as const;

export type TitleFrameKey = keyof typeof zhTitleFrames;
export type TitleFrameCatalog = Record<
  TitleFrameKey,
  TitleContent
>;

export interface TitleContent {
  label: string;
  variations: readonly string[];
}

export interface WelcomeEntry {
  text: string;
  symbols: readonly [GlyphName, GlyphName];
}

export interface PlayfulCatalog {
  welcome: readonly WelcomeEntry[];
  waiting: readonly string[];
}

// Local cultural references are independent entries, not translation keys.
export const zhPlayful: PlayfulCatalog = {
  welcome: [
    { text: "沙发给你留着呢。", symbols: ["couch", "heart"] },
    { text: "单人剧情，也能组队围观。", symbols: ["gamepad", "users"] },
    { text: "三、二、一……好戏开场。", symbols: ["clock", "clapperboard"] },
    { text: "支线任务：分享一点快乐。", symbols: ["branch", "heart"] },
    { text: "零食就位，随时开场。", symbols: ["popcorn", "play"] },
    { text: "下一幕，轮到你了。", symbols: ["clapperboard", "arrowRight"] },
    { text: "存个档，歇一会儿。", symbols: ["save", "moon"] },
    { text: "隐藏成就：有人懂你的快乐。", symbols: ["trophy", "heart"] },
    { text: "前方高能，请自备瓜子。", symbols: ["zap", "popcorn"] },
    { text: "别急着走，片尾还有彩蛋。", symbols: ["clapperboard", "gift"] },
    { text: "这段精彩，值得喊个人来看。", symbols: ["refresh", "users"] },
    { text: "大佬带路，我搬小板凳。", symbols: ["arrowRight", "couch"] },
    { text: "今天不赶主线，随便逛逛。", symbols: ["mountain", "branch"] },
    { text: "有些名场面，得有人见证。", symbols: ["tv", "eye"] },
    { text: "刚研究出个好东西，快来看看。", symbols: ["bulb", "eye"] },
    { text: "一个敢分享，一群敢围观。", symbols: ["cast", "users"] },
    { text: "导演，今天拍点什么？", symbols: ["clapperboard", "play"] },
    { text: "下一关，也叫上朋友吧。", symbols: ["flag", "users"] },
    { text: "分享一下，快乐就多一份。", symbols: ["cast", "heart"] },
    { text: "二十个座位，凑一场热闹。", symbols: ["couch", "users"] },
  ],
  waiting: [
    "先搬个小板凳。",
    "瓜子花生矿泉水。",
    "爆米花要甜的还是咸的？",
    "趁这会儿，伸个懒腰。",
    "先喝口水，吨吨吨。",
    "沙发不会长腿跑掉。",
    "支线任务：把自己坐舒服。",
    "围观也要有仪式感。",
    "像素虽小，戏还挺多。",
    "传说级装备：舒适的小板凳。",
    "镜头之外，也有故事。",
    "猫可以上沙发，键盘先放过。",
    "眨眨眼，小电视也会。",
    "稳住，小板凳已就位。",
    "友情提示：瓜子壳记得扔。",
    "此处应有一段旁白。",
    "被动技能：发呆。",
    "忙里偷个闲。",
    "不如跳舞？坐着也行。",
    "今日存档点：沙发。",
  ],


};
