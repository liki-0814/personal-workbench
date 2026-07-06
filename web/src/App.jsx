import {
  Archive,
  Bot,
  CalendarDays,
  Check,
  ChevronDown,
  ChevronRight,
  Circle,
  Clock3,
  Database,
  Eye,
  EyeOff,
  Folder,
  FolderPlus,
  GitBranch,
  History,
  Inbox,
  Loader2,
  MemoryStick,
  MessageSquare,
  PanelLeftClose,
  Plus,
  RefreshCw,
  Save,
  Trash2,
  Search,
  Send,
  GripVertical,
  Settings,
  ShieldCheck,
  Sparkles,
  SquareStop,
  SquareCheckBig,
  TerminalSquare,
  Wrench,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

const DEFAULT_FOLDERS = [
  { id: "product", name: "Product" },
  { id: "research", name: "Research" },
  { id: "infrastructure", name: "Infrastructure" },
  { id: "personal", name: "Personal" },
  { id: "archive", name: "Archive" },
];

const NAV_ITEMS = [
  { id: "workbench", label: "Workbench", icon: MessageSquare },
  { id: "tasks", label: "Tasks", icon: SquareCheckBig },
  { id: "memory", label: "Memory", icon: MemoryStick },
  { id: "settings", label: "Settings", icon: Settings },
];

const EMPTY_WORKFLOW = {
  resolved_kind: "auto",
  workflow: {
    name: "auto",
    start: "plan",
    nodes: {
      plan: { id: "plan", label: "Plan" },
      execute: { id: "execute", label: "Execute" },
      verify: { id: "verify", label: "Verify" },
      review: { id: "review", label: "Review" },
    },
    edges: [],
  },
};

const CHAT_ROUTE_PLAN = {
  resolved_kind: "chat",
  workflow: {
    name: "chat",
    start: "context",
    nodes: {
      context: { id: "context", label: "Context" },
      respond: { id: "respond", label: "Respond" },
    },
    edges: [],
  },
};

const WORKFLOW_KIND_OPTIONS = [
  { value: "auto", label: "Auto" },
  { value: "chat", label: "Chat" },
  { value: "code", label: "Code" },
  { value: "research", label: "Research" },
  { value: "ops", label: "Ops" },
  { value: "general", label: "General" },
];

const AGENT_WORKFLOW_KINDS = new Set(["code"]);

const WORKFLOW_LABELS = {
  plan: "Plan",
  approve_plan: "Approve",
  execute: "Execute",
  verify: "Verify",
  review: "Review",
  synthesize: "Synthesize",
  verify_sources: "Verify",
  final_report: "Finalize",
  web_search: "Search",
  read_papers: "Read Papers",
  adaptive_research: "Adaptive Research",
  local_context: "Context",
  context: "Context",
  respond: "Respond",
  end: "Done",
};

const CODE_AGENT_INTENT_PATTERNS = [
  /(代码|代码库|仓库|前端|后端|组件|接口|函数|模块|编译|构建|单测|报错|bug|调试|stack trace|diff|patch|PR|pull request)/i,
  /(阅读|理解|解释|分析|检查|审查|修改|修复|重构|实现|新增|优化).*(代码|代码库|仓库|文件|组件|函数|模块|接口|页面|网页|前端|后端|UI|diff|patch|PR|pull request)/i,
  /(运行|跑).*(测试|构建|编译|lint|typecheck)/i,
  /\b(code|codebase|repo|repository|function|module|component|api|debug|bug|stack trace|refactor|test|tests|testing|build|compile|lint|typecheck|diff|patch|commit|pr|pull request|frontend|backend|react|rust|vite)\b/i,
  /\b(implement|fix|change|update|add|review|analyze|explain|read)\b.*\b(code|codebase|repo|repository|file|function|module|component|api|bug|test|build|ui|page|frontend|backend|diff|patch|pr|pull request)\b/i,
];

const SIMPLE_CHAT_PATTERNS = [
  /^(hi|hello|hey|yo|thanks|thank you|thx)\b/i,
  /^(你好|您好|嗨|在吗|谢谢|多谢|早上好|下午好|晚上好)/,
  /(你是谁|介绍一下你自己|介绍你自己|你能帮我做什么|你可以帮我做什么|你会做什么|你有什么能力|what can you do|who are you)/i,
];

const RESEARCH_INTENT_PATTERNS = [
  /(调研|研究|资料|竞品|论文|搜索|对比|资料来源|文献)/,
  /\b(research|investigate|latest|compare|survey|paper|papers|source|sources|market|literature)\b/i,
];

const OPS_INTENT_PATTERNS = [
  /(服务器|部署|远程|日志|运维|巡检|告警|监控|发布|维护|ssh)/i,
  /\b(ssh|server|deploy|deployment|ops|operation|operations|infra|infrastructure|incident|logs|remote|maintenance|runbook|monitor|monitoring|restart|service|production)\b/i,
];

function shouldUseCodeAgent(text) {
  const normalized = text.trim().replace(/\s+/g, " ");
  if (!normalized) return false;
  return CODE_AGENT_INTENT_PATTERNS.some((pattern) => pattern.test(normalized));
}

function resolvePromptRoute(text, selectedKind) {
  const normalized = text.trim().replace(/\s+/g, " ");
  if (selectedKind === "chat") return { route: "chat", kind: "chat" };
  if (selectedKind && selectedKind !== "auto") return { route: "workflow", kind: selectedKind };
  if (SIMPLE_CHAT_PATTERNS.some((pattern) => pattern.test(normalized))) {
    return { route: "chat", kind: "chat" };
  }
  if (shouldUseCodeAgent(normalized)) return { route: "workflow", kind: "code" };
  if (RESEARCH_INTENT_PATTERNS.some((pattern) => pattern.test(normalized))) {
    return { route: "workflow", kind: "research" };
  }
  if (OPS_INTENT_PATTERNS.some((pattern) => pattern.test(normalized))) {
    return { route: "workflow", kind: "ops" };
  }
  return { route: "workflow", kind: "general" };
}

const EMPTY_TASKS = {
  active_task_id: null,
  tasks: [],
};

const PERSONAL_TASKS_STORAGE_KEY = "pw.personal_tasks.v1";

const TASK_GROUPS = [
  { key: "overdue", title: "逾期", defaultOpen: true },
  { key: "today", title: "今日", defaultOpen: true },
  { key: "thisweek", title: "本周", defaultOpen: true },
  { key: "later", title: "之后", defaultOpen: false },
  { key: "no_date", title: "无日期", defaultOpen: false },
  { key: "completed", title: "已完成", defaultOpen: false },
];

const EMPTY_MEMORY = {
  graph: {
    facts: 0,
    hnsw_nodes: 0,
    hnsw_edges: 0,
    vectors: 0,
    max_layer: 0,
    entry_point: null,
  },
  facts: [],
  inferences: [],
  hypotheses: [],
  candidates: [],
};

function getApiBaseCandidates() {
  const queryParams = new URLSearchParams(window.location.search);
  const candidates = [];
  const queryBase = queryParams.get("api");
  const envBase = import.meta.env?.VITE_PWCLI_API || "";
  if (queryBase) {
    candidates.push(queryBase);
  }
  if (envBase) {
    candidates.push(envBase);
  }
  if (window.location.origin) {
    candidates.push(window.location.origin);
  }
  if (window.location.port !== "8791") {
    candidates.push("http://127.0.0.1:8791");
    candidates.push("http://localhost:8791");
  }
  const seen = new Set();
  return candidates
    .map((item) => String(item || "").replace(/\/+$/, ""))
    .filter(Boolean)
    .filter((item) => {
      if (seen.has(item)) return false;
      seen.add(item);
      return true;
    });
}

const API_BASES = getApiBaseCandidates();

function apiUrl(path, base) {
  const normalized = path.startsWith("/") ? path : `/${path}`;
  if (!base) {
    return normalized;
  }
  if (base.startsWith("http://") || base.startsWith("https://")) {
    return `${base}${normalized}`;
  }
  return normalized;
}

function storageGet(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    return raw ? JSON.parse(raw) : fallback;
  } catch {
    return fallback;
  }
}

async function apiFetch(path, options) {
  let lastError;
  for (let index = 0; index < API_BASES.length; index += 1) {
    const base = API_BASES[index];
    try {
      const response = await fetch(apiUrl(path, base), {
        headers: { "content-type": "application/json" },
        ...options,
      });
      if (!response.ok) {
        const text = await response.text();
        let message = text;
        try {
          message = JSON.parse(text).error || text;
        } catch {
          // Keep the plain response text.
        }
        const finalMessage = `${message || `${response.status} ${response.statusText}`}${
          base ? ` (${base})` : ""
        }`;
        if (index < API_BASES.length - 1) {
          lastError = new Error(finalMessage);
          continue;
        }
        throw new Error(finalMessage);
      }
      return response.json();
    } catch (error) {
      lastError = error instanceof Error ? error : new Error(String(error));
      if (base) {
        lastError = new Error(`${lastError.message} (${base})`);
      }
      if ((error instanceof TypeError || error instanceof SyntaxError) && index < API_BASES.length - 1) {
        continue;
      }
      throw lastError;
    }
  }
  throw new Error(lastError?.message || "Failed to fetch");
}

function apiEventSource(path) {
  const candidates = API_BASES;
  const targetBase = candidates.find((base) =>
    base === window.location.origin || base.includes("127.0.0.1") || base.includes("localhost"),
  );
  return new EventSource(apiUrl(path, targetBase));
}

function compactThreadTitle(entry) {
  return entry?.user_preview || entry?.id || "Untitled thread";
}

function formatSessionTime(ms) {
  if (!ms) return "";
  const date = new Date(ms);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (date.toDateString() === today.toDateString()) return "Today";
  if (date.toDateString() === yesterday.toDateString()) return "Yesterday";
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function storageSet(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Local storage is nice-to-have for folder preferences.
  }
}

function usePersistentState(key, fallback) {
  const [value, setValue] = useState(() => storageGet(key, fallback));
  useEffect(() => storageSet(key, value), [key, value]);
  return [value, setValue];
}

function workflowKindLabel(kind) {
  return kind || "auto";
}

function shortId(id) {
  if (!id) return "";
  return id.length > 12 ? `${id.slice(0, 12)}...` : id;
}

function clipText(value, max = 180) {
  const text = String(value || "").replace(/\s+/g, " ").trim();
  return text.length > max ? `${text.slice(0, max - 1)}...` : text;
}

function markdownToPlainText(markdown) {
  return String(markdown || "")
    .replace(/```[\s\S]*?```/g, (block) => block.replace(/```[a-zA-Z0-9_-]*\n?/g, "").replace(/```/g, ""))
    .replace(/`([^`]+)`/g, "$1")
    .replace(/!\[([^\]]*)\]\([^)]+\)/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^\s*[-*+]\s+/gm, "")
    .replace(/^\s*\d+\.\s+/gm, "")
    .replace(/[*_~]{1,3}/g, "")
    .replace(/>\s?/g, "")
    .trim();
}

async function copyTextToClipboard(text) {
  const value = String(text || "");
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value);
    return;
  }
  const textarea = document.createElement("textarea");
  textarea.value = value;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  document.execCommand("copy");
  document.body.removeChild(textarea);
}

function statusClass(status) {
  return String(status || "pending").toLowerCase();
}

function makePersonalTaskId() {
  return `todo_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
}

function formatDateInput(date) {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function parseRelativeDateToken(token) {
  const today = new Date();
  const normalized = token.trim();
  if (normalized === "今天" || normalized === "今日") return formatDateInput(today);
  if (normalized === "明天") {
    const next = new Date(today);
    next.setDate(today.getDate() + 1);
    return formatDateInput(next);
  }
  if (normalized === "后天") {
    const next = new Date(today);
    next.setDate(today.getDate() + 2);
    return formatDateInput(next);
  }
  if (normalized === "大后天") {
    const next = new Date(today);
    next.setDate(today.getDate() + 3);
    return formatDateInput(next);
  }
  if (normalized === "下周") {
    const next = new Date(today);
    const day = today.getDay();
    next.setDate(today.getDate() + (day === 0 ? 1 : 8 - day));
    return formatDateInput(next);
  }
  if (/^\d{4}-\d{2}-\d{2}$/.test(normalized)) return normalized;
  if (/^\d{1,2}\/\d{1,2}$/.test(normalized)) {
    const [month, day] = normalized.split("/").map(Number);
    return `${today.getFullYear()}-${String(month).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
  }
  return "";
}

function parseQuickTaskInput(input) {
  let title = input.trim();
  let priority = "medium";
  let dueDate = "";
  const priorityMatch = title.match(/#(高|中|低|h|m|l|high|medium|low)\b/i);
  if (priorityMatch) {
    const token = priorityMatch[1].toLowerCase();
    if (token === "高" || token === "h" || token === "high") priority = "high";
    if (token === "低" || token === "l" || token === "low") priority = "low";
    title = title.replace(priorityMatch[0], "").trim();
  }
  const dueMatch = title.match(/@(今天|今日|明天|后天|大后天|下周|\d{1,2}\/\d{1,2}|\d{4}-\d{2}-\d{2})/);
  if (dueMatch) {
    dueDate = parseRelativeDateToken(dueMatch[1]);
    title = title.replace(dueMatch[0], "").trim();
  }
  return { title, priority, dueDate };
}

function normalizePersonalTask(raw, index = 0) {
  const now = new Date().toISOString();
  const title = String(raw?.title || "").trim() || "Untitled task";
  const status = ["todo", "in_progress", "done"].includes(raw?.status) ? raw.status : "todo";
  const priority = ["low", "medium", "high"].includes(raw?.priority) ? raw.priority : "medium";
  const type = ["today", "week", "longterm"].includes(raw?.type) ? raw.type : "today";
  return {
    id: raw?.id || raw?.task_id || makePersonalTaskId(),
    title,
    status,
    priority,
    type,
    createdAt: raw?.createdAt || raw?.created_at || now,
    updatedAt: raw?.updatedAt || raw?.updated_at || now,
    completedAt: raw?.completedAt || undefined,
    order: Number.isFinite(Number(raw?.order)) ? Number(raw.order) : index,
    dueDate: raw?.dueDate || "",
    scheduledStart: raw?.scheduledStart || "",
    scheduledEnd: raw?.scheduledEnd || "",
    notes: raw?.notes || "",
    subTasks: Array.isArray(raw?.subTasks)
      ? raw.subTasks.map((step, stepIndex) => ({
          id: step.id || makePersonalTaskId(),
          title: String(step.title || step.label || "").trim() || `Step ${stepIndex + 1}`,
          completed: Boolean(step.completed),
          dueDate: step.dueDate || "",
          scheduledStart: step.scheduledStart || "",
          scheduledEnd: step.scheduledEnd || "",
        }))
      : [],
  };
}

function normalizePersonalTaskData(data) {
  const rawTasks = Array.isArray(data) ? data : Array.isArray(data?.tasks) ? data.tasks : [];
  return {
    active_task_id: null,
    tasks: rawTasks.map(normalizePersonalTask).sort((a, b) => (a.order ?? 0) - (b.order ?? 0)),
  };
}

function createPersonalTaskFromParsed(parsed) {
  const now = new Date().toISOString();
  const dueDate = parsed.dueDate || "";
  return {
    id: makePersonalTaskId(),
    title: parsed.title.trim(),
    status: "todo",
    priority: parsed.priority || "medium",
    type: dueDate ? "today" : "today",
    createdAt: now,
    updatedAt: now,
    order: Date.now(),
    dueDate,
    scheduledStart: parsed.scheduledStart || "",
    scheduledEnd: parsed.scheduledEnd || "",
    notes: parsed.notes || "",
    subTasks: (parsed.subTasks || [])
      .map((step) => ({
        id: makePersonalTaskId(),
        title: String(step.title || "").trim(),
        completed: false,
        dueDate: "",
        scheduledStart: "",
        scheduledEnd: "",
      }))
      .filter((step) => step.title),
  };
}

function taskDone(task) {
  return task.status === "done";
}

function taskGroupKey(task) {
  if (taskDone(task)) return "completed";
  if (task.dueDate) {
    const due = new Date(`${task.dueDate}T00:00:00`);
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    if (due < today) return "overdue";
    if (due.toDateString() === today.toDateString()) return "today";
    const endOfWeek = new Date(today);
    endOfWeek.setDate(today.getDate() + (7 - today.getDay()));
    endOfWeek.setHours(23, 59, 59, 999);
    if (due <= endOfWeek) return "thisweek";
    return "later";
  }
  if (task.type === "today") return "today";
  if (task.type === "week") return "thisweek";
  return "no_date";
}

function groupPersonalTasks(tasks) {
  return TASK_GROUPS.map((group) => ({
    ...group,
    tasks: tasks.filter((task) => taskGroupKey(task) === group.key),
  })).filter((group) => group.tasks.length > 0);
}

function relativeTaskDateLabel(dateValue) {
  if (!dateValue) return "";
  const due = new Date(`${dateValue}T00:00:00`);
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const diff = Math.round((due.getTime() - today.getTime()) / 86400000);
  if (diff < 0) return `逾期 ${Math.abs(diff)} 天`;
  if (diff === 0) return "今天";
  if (diff === 1) return "明天";
  if (diff === 2) return "后天";
  if (diff < 7) return ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][due.getDay()];
  return `${due.getMonth() + 1}/${due.getDate()}`;
}

function nodeLabel(id, node) {
  return node?.label || WORKFLOW_LABELS[id] || id.replace(/_/g, " ");
}

function friendlyToolLabel(toolId, name) {
  const key = String(toolId || name || "").toLowerCase().replace(/[._-]/g, "_");
  if (key.includes("anysearch")) return "Search Web";
  if (key.includes("web_fetch")) return "Read Web";
  if (key.includes("mineru")) return "Parse PDF";
  if (key.includes("local_file_index")) return "Search Local Context";
  const raw = String(name || toolId || "Tool").replace(/^builtin[._-]/, "");
  return raw.replace(/[._-]+/g, " ").replace(/\b\w/g, (char) => char.toUpperCase());
}

function dynamicNodeId(callId) {
  return `dynamic_${String(callId || Date.now()).replace(/[^a-zA-Z0-9_:-]/g, "_").slice(0, 72)}`;
}

function orderedWorkflowNodes(plan) {
  const workflow = plan?.workflow || EMPTY_WORKFLOW.workflow;
  const nodes = workflow.nodes || {};
  const dynamicOrder = Array.isArray(plan?.dynamic_order) ? plan.dynamic_order : [];
  const preferred = [
    "plan",
    "approve_plan",
    "adaptive_research",
    ...dynamicOrder,
    "web_search",
    "read_papers",
    "local_context",
    "execute",
    "verify",
    "synthesize",
    "verify_sources",
    "final_report",
    "review",
  ];
  const order = [];
  let current = workflow.start || "plan";
  const seen = new Set();
  while (current && nodes[current] && !seen.has(current)) {
    seen.add(current);
    order.push(current);
    const edge = (workflow.edges || []).find((item) => item.from === current);
    current = edge?.to;
  }
  if (order.length <= 1) {
    const fallback = preferred.filter((id) => nodes[id]);
    return fallback.length > 0 ? fallback.slice(0, 7) : Object.keys(nodes).slice(0, 7);
  }
  return order.filter((id) => id !== "end").slice(0, 7);
}

function eventData(event) {
  try {
    return JSON.parse(event.data);
  } catch {
    return null;
  }
}

function unwrapRuntimeEvent(event) {
  if (!event || typeof event !== "object") return null;
  if (event.type) return event;
  const [type, payload] = Object.entries(event)[0] || [];
  if (!type) return null;
  if (payload && typeof payload === "object") return { type, ...payload };
  return { type, value: payload };
}

function eventLine(event) {
  const unwrapped = unwrapRuntimeEvent(event) || event;
  if (!unwrapped) return "";
  if (typeof unwrapped === "string") return unwrapped;
  if (unwrapped.type === "Started" || unwrapped.type === "Completed") return "";
  return (
    unwrapped.message ||
    unwrapped.chunk ||
    unwrapped.stream ||
    unwrapped.error ||
    unwrapped.status ||
    JSON.stringify(unwrapped)
  );
}

function App() {
  const [activeNav, setActiveNav] = useState("workbench");
  const [historyOpen, setHistoryOpen] = usePersistentState("pw.historyOpen", true);
  const [historyWidth, setHistoryWidth] = usePersistentState("pw.historyWidth", 304);
  const [folders, setFolders] = useState(DEFAULT_FOLDERS);
  const [folderOpen, setFolderOpen] = usePersistentState("pw.folderOpen", { product: true });
  const [folderAssignments, setFolderAssignments] = useState({});
  const [historyTab, setHistoryTab] = useState("folders");
  const [search, setSearch] = useState("");
  const [draggingSessionId, setDraggingSessionId] = useState(null);
  const [sessions, setSessions] = useState([]);
  const [activeSession, setActiveSession] = useState(null);
  const [prompt, setPrompt] = useState("");
  const [workflowPlan, setWorkflowPlan] = useState(EMPTY_WORKFLOW);
  const [workflowKind, setWorkflowKind] = usePersistentState("pwcli.chat.workflow_kind", "auto");
  const [activeNode, setActiveNode] = useState(null);
  const [workflowNodeState, setWorkflowNodeState] = useState({});
  const [workflowTaskId, setWorkflowTaskId] = useState(null);
  const [workflowMaterials, setWorkflowMaterials] = useState(null);
  const [workflowMaterialsLoading, setWorkflowMaterialsLoading] = useState(false);
  const [openMaterial, setOpenMaterial] = useState(null);
  const [runState, setRunState] = useState("idle");
  const [events, setEvents] = useState([]);
  const [showThinking, setShowThinking] = useState(false);
  const [status, setStatus] = useState(null);
  const [tasksData, setTasksData] = usePersistentState(PERSONAL_TASKS_STORAGE_KEY, EMPTY_TASKS);
  const [memoryData, setMemoryData] = useState(EMPTY_MEMORY);
  const [settingsDraft, setSettingsDraft] = useState(null);
  const [taskCreateSubmitting, setTaskCreateSubmitting] = useState(false);
  const [taskDecomposeBusy, setTaskDecomposeBusy] = useState({});
  const [sessionLoadingId, setSessionLoadingId] = useState(null);
  const [notice, setNotice] = useState(null);
  const [settingsSaving, setSettingsSaving] = useState(false);
  const [messages, setMessages] = useState([
    {
      id: "welcome",
      role: "assistant",
      content: "Tell pw what you want to do. It will plan the route, run the graph, and keep the execution visible without taking over the page.",
      thinking: "",
      tools: [],
    },
  ]);
  const [thinkingOpen, setThinkingOpen] = useState(false);
  const [composerFocused, setComposerFocused] = useState(false);
  const eventSourceRef = useRef(null);
  const activeWorkflowTaskRef = useRef(null);
  const activeNodeRef = useRef(null);
  const workflowPlanRef = useRef(workflowPlan);
  const workflowToolCallNodeRef = useRef(new Map());
  const workflowPollRef = useRef(null);
  const workflowChildTaskIdsRef = useRef(new Set());
  const runStateRef = useRef(runState);
  const messagesEndRef = useRef(null);

  useEffect(() => {
    loadSessions();
    loadSessionFolders();
    loadStatus();
    return () => {
      eventSourceRef.current?.close();
      if (workflowPollRef.current) {
        clearInterval(workflowPollRef.current);
      }
    };
  }, []);

  useEffect(() => {
    if (activeNav === "tasks") loadTasks();
    if (activeNav === "memory") loadMemory();
    if (activeNav === "settings") loadStatus();
  }, [activeNav]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ block: "end", behavior: "smooth" });
  }, [messages, events.length]);

  useEffect(() => {
    runStateRef.current = runState;
  }, [runState]);

  useEffect(() => {
    activeNodeRef.current = activeNode;
  }, [activeNode]);

  useEffect(() => {
    workflowPlanRef.current = workflowPlan;
  }, [workflowPlan]);

  useEffect(() => {
    if (!notice) return undefined;
    const timer = setTimeout(() => setNotice(null), 2600);
    return () => clearTimeout(timer);
  }, [notice]);

  const filteredSessions = useMemo(() => {
    const query = search.trim().toLowerCase();
    if (!query) return sessions;
    return sessions.filter((entry) => {
      const text = `${entry.id} ${entry.user_preview} ${entry.assistant_preview}`.toLowerCase();
      return text.includes(query);
    });
  }, [sessions, search]);

  const sessionsByFolder = useMemo(() => {
    const grouped = new Map();
    for (const folder of folders) grouped.set(folder.id, []);
    for (const entry of filteredSessions) {
      const folderId = folderAssignments[entry.id] || (folders[0]?.id ?? "product");
      if (!grouped.has(folderId)) grouped.set(folderId, []);
      grouped.get(folderId).push(entry);
    }
    return grouped;
  }, [filteredSessions, folderAssignments, folders]);

  const workflowNodes = useMemo(() => orderedWorkflowNodes(workflowPlan), [workflowPlan]);

  async function loadSessions() {
    try {
      const data = await apiFetch("/api/sessions");
      setSessions(Array.isArray(data.sessions) ? data.sessions : []);
    } catch {
      setSessions([]);
    }
  }

  async function loadSessionFolders() {
    try {
      const data = await apiFetch("/api/session-folders");
      if (Array.isArray(data.folders) && data.folders.length > 0) {
        setFolders(data.folders);
      }
      setFolderAssignments(data.assignments || {});
    } catch {
      setFolders(DEFAULT_FOLDERS);
      setFolderAssignments({});
    }
  }

  async function loadStatus() {
    try {
      const [statusData, configData] = await Promise.all([
        apiFetch("/api/status"),
        apiFetch("/api/config"),
      ]);
      setStatus(statusData);
      setShowThinking(Boolean(configData.show_thinking));
      setSettingsDraft(configData);
    } catch {
      setStatus(null);
      setShowThinking(false);
    }
  }

  async function loadTasks() {
    setTasksData((prev) => normalizePersonalTaskData(prev));
  }

  async function createTask(title) {
    const trimmed = title.trim();
    if (!trimmed) return false;
    setTaskCreateSubmitting(true);
    try {
      const parsed = parseQuickTaskInput(trimmed);
      if (!parsed.title) throw new Error("Task title is required.");
      const task = createPersonalTaskFromParsed({
        title: parsed.title,
        notes: "",
        priority: parsed.priority,
        dueDate: parsed.dueDate,
        subTasks: [],
      });
      setTasksData((prev) => {
        const current = normalizePersonalTaskData(prev);
        return { active_task_id: null, tasks: [task, ...current.tasks] };
      });
      setNotice({ kind: "ok", text: `Created task: ${task.title}` });
      return true;
    } catch (error) {
      setNotice({ kind: "error", text: `Could not create task: ${error.message}` });
      return false;
    } finally {
      setTaskCreateSubmitting(false);
    }
  }

  async function createTaskAndDecompose(title) {
    const trimmed = title.trim();
    if (!trimmed) return false;
    setTaskCreateSubmitting(true);
    try {
      const data = await apiFetch("/api/task-parser", {
        method: "POST",
        body: JSON.stringify({ input: trimmed }),
      });
      const task = createPersonalTaskFromParsed({
        title: String(data.title || "").trim() || trimmed,
        notes: data.notes || "",
        priority: ["low", "medium", "high"].includes(data.priority) ? data.priority : "medium",
        dueDate: data.dueDate || "",
        scheduledStart: data.scheduledStart || "",
        scheduledEnd: data.scheduledEnd || "",
        subTasks: Array.isArray(data.subTasks) ? data.subTasks : [],
      });
      setTasksData((prev) => {
        const current = normalizePersonalTaskData(prev);
        return { active_task_id: null, tasks: [task, ...current.tasks] };
      });
      setNotice({ kind: "ok", text: `AI 拆解完成: ${task.subTasks.length} 个子步骤` });
      return true;
    } catch (error) {
      setNotice({ kind: "error", text: `Could not create and decompose task: ${error.message}` });
      return false;
    } finally {
      setTaskCreateSubmitting(false);
    }
  }

  async function decomposeTask(taskId, { goal = "" } = {}) {
    if (!taskId) return false;
    setTaskDecomposeBusy((prev) => ({ ...prev, [taskId]: true }));
    try {
      const data = await apiFetch("/api/task-parser", {
        method: "POST",
        body: JSON.stringify({ input: goal.trim() || "拆解这个任务" }),
      });
      const subTasks = (Array.isArray(data.subTasks) ? data.subTasks : []).map((step) => ({
        id: makePersonalTaskId(),
        title: String(step.title || "").trim(),
        completed: false,
        dueDate: "",
        scheduledStart: "",
        scheduledEnd: "",
      })).filter((step) => step.title);
      setTasksData((prev) => {
        const current = normalizePersonalTaskData(prev);
        return {
          active_task_id: null,
          tasks: current.tasks.map((task) =>
            task.id === taskId
              ? {
                  ...task,
                  notes: data.notes || task.notes,
                  priority: ["low", "medium", "high"].includes(data.priority) ? data.priority : task.priority,
                  dueDate: data.dueDate || task.dueDate,
                  scheduledStart: data.scheduledStart || task.scheduledStart,
                  scheduledEnd: data.scheduledEnd || task.scheduledEnd,
                  subTasks,
                  updatedAt: new Date().toISOString(),
                }
              : task,
          ),
        };
      });
      setNotice({ kind: "ok", text: `AI 拆解完成: ${subTasks.length} 个子步骤` });
      return true;
    } catch (error) {
      setNotice({ kind: "error", text: `Could not decompose task: ${error.message}` });
      return false;
    } finally {
      setTaskDecomposeBusy((prev) => {
        const next = { ...prev };
        delete next[taskId];
        return next;
      });
    }
  }

  async function deleteTask(taskId, taskTitle) {
    const title = (taskTitle && taskTitle.trim()) || shortId(taskId);
    if (!window.confirm(`Delete task "${title}"?`)) return false;
    setTasksData((prev) => {
      const current = normalizePersonalTaskData(prev);
      return { active_task_id: null, tasks: current.tasks.filter((task) => task.id !== taskId) };
    });
    setNotice({ kind: "ok", text: `Deleted task: ${title}` });
    return true;
  }

  async function loadMemory() {
    try {
      const [graphData, layersData, inboxData] = await Promise.all([
        apiFetch("/api/memory/graph"),
        apiFetch("/api/memory/layers"),
        apiFetch("/api/memory/inbox"),
      ]);
      setMemoryData({
        graph: graphData.graph || EMPTY_MEMORY.graph,
        facts: Array.isArray(layersData.facts) ? layersData.facts : [],
        inferences: Array.isArray(layersData.inferences) ? layersData.inferences : [],
        hypotheses: Array.isArray(layersData.hypotheses) ? layersData.hypotheses : [],
        candidates: Array.isArray(inboxData.candidates) ? inboxData.candidates : [],
      });
    } catch (error) {
      setMemoryData(EMPTY_MEMORY);
    }
  }

  async function toggleShowThinking() {
    const next = !showThinking;
    setShowThinking(next);
    try {
      const data = await apiFetch("/api/config/provider-model", {
        method: "POST",
        body: JSON.stringify({ show_thinking: next }),
      });
      setShowThinking(Boolean(data.show_thinking));
    } catch (error) {
      setShowThinking(!next);
      setNotice({ kind: "error", text: `Could not update thinking display: ${error.message}` });
    }
  }

  async function addFolder(name) {
    const trimmed = name?.trim();
    if (!trimmed) return false;
    try {
      const state = await apiFetch("/api/session-folders", {
        method: "POST",
        body: JSON.stringify({ name: trimmed }),
      });
      setFolders(state.folders || DEFAULT_FOLDERS);
      setFolderAssignments(state.assignments || {});
      const created = state.folders?.[state.folders.length - 1];
      if (created?.id) {
        setFolderOpen((prev) => ({ ...prev, [created.id]: true }));
      }
      setNotice({ kind: "ok", text: `Folder created: ${trimmed}` });
      return true;
    } catch (error) {
      setNotice({ kind: "error", text: `Could not create folder: ${error.message}` });
      return false;
    }
  }

  async function assignSession(entryId, folderId) {
    try {
      const state = await apiFetch(`/api/sessions/${encodeURIComponent(entryId)}/folder`, {
        method: "POST",
        body: JSON.stringify({ folder_id: folderId }),
      });
      setFolders(state.folders || folders);
      setFolderAssignments(state.assignments || {});
    } catch (error) {
      setNotice({ kind: "error", text: `Could not move chat: ${error.message}` });
    }
  }

  async function deleteSession(entryId) {
    const finalizeDelete = async () => {
      setSessions((prev) => prev.filter((entry) => entry.id !== entryId));
      setFolderAssignments((prev) => {
        if (!prev[entryId]) return prev;
        const next = { ...prev };
        delete next[entryId];
        return next;
      });
      if (activeSession?.id === entryId) {
        setActiveSession(null);
        setMessages([
          {
            id: "welcome",
            role: "assistant",
            content:
              "Tell pw what you want to do. It will plan the route, run the graph, and keep the execution visible without taking over the page.",
            thinking: "",
            tools: [],
          },
        ]);
      }
      await loadSessionFolders();
      setNotice({ kind: "ok", text: `Deleted chat ${shortId(entryId)}` });
    };

    try {
      await apiFetch(`/api/sessions/${encodeURIComponent(entryId)}`, {
        method: "DELETE",
      });
      await finalizeDelete();
      return;
    } catch (error) {
      if (error.message.includes("405")) {
        try {
          await apiFetch(`/api/sessions/${encodeURIComponent(entryId)}`, {
            method: "POST",
          });
          await finalizeDelete();
          return;
        } catch (fallbackError) {
          setNotice({ kind: "error", text: `Could not delete chat (backend method not supported): ${fallbackError.message}` });
          return;
        }
      }
      setNotice({ kind: "error", text: `Could not delete chat: ${error.message}` });
    }
  }

  function updateTask(taskId, patch) {
    setTasksData((prev) => {
      const current = normalizePersonalTaskData(prev);
      return {
        active_task_id: null,
        tasks: current.tasks.map((task) =>
          task.id === taskId ? { ...task, ...patch, updatedAt: new Date().toISOString() } : task,
        ),
      };
    });
  }

  function cycleTaskStatus(taskId) {
    setTasksData((prev) => {
      const current = normalizePersonalTaskData(prev);
      return {
        active_task_id: null,
        tasks: current.tasks.map((task) => {
          if (task.id !== taskId) return task;
          const nextStatus =
            task.status === "todo" ? "in_progress" : task.status === "in_progress" ? "done" : "todo";
          return {
            ...task,
            status: nextStatus,
            completedAt: nextStatus === "done" ? new Date().toISOString() : undefined,
            updatedAt: new Date().toISOString(),
          };
        }),
      };
    });
  }

  async function saveSettingsDraft() {
    if (!settingsDraft) return;
    setSettingsSaving(true);
    try {
      const data = await apiFetch("/api/config", {
        method: "PUT",
        body: JSON.stringify(settingsDraft),
      });
      setShowThinking(Boolean(data.show_thinking));
      await loadStatus();
      setNotice({ kind: "ok", text: "Settings saved" });
    } catch (error) {
      setNotice({ kind: "error", text: `Could not save settings: ${error.message}` });
    } finally {
      setSettingsSaving(false);
    }
  }

  const startResize = useCallback(
    (event) => {
      event.preventDefault();
      const startX = event.clientX;
      const startWidth = historyWidth;
      const onMove = (moveEvent) => {
        const next = Math.min(420, Math.max(236, startWidth + moveEvent.clientX - startX));
        setHistoryWidth(next);
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [historyWidth, setHistoryWidth],
  );

  async function sendPrompt() {
    const text = prompt.trim();
    if (!text || ["planning", "running", "waiting", "stopping"].includes(runStateRef.current)) return;
    const resolvedRoute = resolvePromptRoute(text, workflowKind);
    const route = resolvedRoute.route;
    const requestKind = resolvedRoute.kind === "chat" ? "chat" : resolvedRoute.kind;
    eventSourceRef.current?.close();
    if (workflowPollRef.current) {
      clearInterval(workflowPollRef.current);
      workflowPollRef.current = null;
    }
    setPrompt("");
    setRunState(route === "workflow" ? "planning" : "running");
    setActiveNode(route === "workflow" ? "plan" : "respond");
    activeNodeRef.current = route === "workflow" ? "plan" : "respond";
    setWorkflowNodeState(route === "workflow" ? {} : { context: "running", respond: "pending" });
    activeWorkflowTaskRef.current = null;
    setWorkflowTaskId(null);
    setWorkflowMaterials(null);
    setOpenMaterial(null);
    workflowChildTaskIdsRef.current = new Set();
    workflowToolCallNodeRef.current = new Map();
    setEvents([]);
    setMessages((prev) => [
      ...prev,
      { id: `user-${Date.now()}`, role: "user", content: text },
      { id: `assistant-${Date.now()}`, role: "assistant", content: "", thinking: "", tools: [] },
    ]);

    if (route === "chat") {
      setWorkflowPlan(CHAT_ROUTE_PLAN);
      try {
        const created = await apiFetch("/api/chat/runs", {
          method: "POST",
          body: JSON.stringify({ prompt: text, session: activeSession?.id || null }),
        });
        subscribeRun(created.run_id);
      } catch (error) {
        setRunState("failed");
        setWorkflowNodeState({ context: "error", respond: "error" });
        appendAssistant({ content: `Could not start chat: ${error.message}` });
      }
      return;
    }

    try {
      const plan = await apiFetch("/api/workflows/plan", {
        method: "POST",
        body: JSON.stringify({ goal: text, kind: requestKind }),
      });
      setWorkflowPlan(plan);
      workflowPlanRef.current = plan;
    } catch {
      setWorkflowPlan(EMPTY_WORKFLOW);
      workflowPlanRef.current = EMPTY_WORKFLOW;
    }

    try {
      const created = await apiFetch("/api/workflows/runs", {
        method: "POST",
        body: JSON.stringify({
          goal: text,
          kind: requestKind,
          auto_approve: false,
        }),
      });
      const taskId = created.task_id || created.run_id;
      setWorkflowPlan(created);
      workflowPlanRef.current = created;
      activeWorkflowTaskRef.current = taskId;
      setWorkflowTaskId(taskId);
      setRunState("running");
      beginWorkflowRunMonitor(taskId);
      subscribeWorkflow(taskId);
    } catch (error) {
      setRunState("failed");
      activeWorkflowTaskRef.current = null;
      appendAssistant({ content: `Could not start run: ${error.message}` });
    }
  }

  function beginWorkflowRunMonitor(taskId) {
    if (!taskId) return;
    if (workflowPollRef.current) {
      clearInterval(workflowPollRef.current);
      workflowPollRef.current = null;
    }
    let consecutiveErrors = 0;
    workflowPollRef.current = setInterval(async () => {
      if (!activeWorkflowTaskRef.current || activeWorkflowTaskRef.current !== taskId) return;
      if (["idle", "completed", "failed"].includes(runStateRef.current)) {
        clearInterval(workflowPollRef.current);
        workflowPollRef.current = null;
        return;
      }
      try {
        const data = await apiFetch(`/api/tasks/${encodeURIComponent(taskId)}`);
        consecutiveErrors = 0;
        const status = String(data?.task?.status || "").toLowerCase();
        const workflowStatus = String(
          data?.task?.metadata?.workflow?.status || data?.workflow_status || "",
        ).toLowerCase();
        if (status && !["running", "pending"].includes(status)) {
          clearInterval(workflowPollRef.current);
          workflowPollRef.current = null;
          if (status === "completed") {
            setRunState("completed");
            setActiveNode("end");
            settleWorkflowSummary(data?.workflow_summary, "completed");
            const finalContent = workflowFinalContent(data?.workflow_summary);
            appendAssistant({
              content: finalContent
                ? `\n\n${finalContent}\n\n---\nWorkflow completed. Task ${taskId} is saved in pwcli tasks.`
                : `\n\nWorkflow completed. Task ${taskId} is saved in pwcli tasks.`,
            });
            loadWorkflowMaterials(taskId);
            clearWorkflowWatch();
            eventSourceRef.current?.close();
          } else {
            setRunState("failed");
            appendAssistant({
              content: `\n\nWorkflow stopped: ${status}${data?.next ? ` - ${String(data.next).split("\n").pop()}` : ""}`,
            });
            clearWorkflowWatch();
            eventSourceRef.current?.close();
          }
          loadSessions();
          activeWorkflowTaskRef.current = null;
          return;
        }

        if (status === "pending" && workflowStatus === "interrupted") {
          clearInterval(workflowPollRef.current);
          workflowPollRef.current = null;
          setRunState("failed");
          appendAssistant({
            content: `\n\nWorkflow interrupted by policy or user decision. ${data?.next || ""}`.trim(),
          });
          clearWorkflowWatch();
          eventSourceRef.current?.close();
          loadSessions();
          activeWorkflowTaskRef.current = null;
        }
      } catch {
        consecutiveErrors += 1;
        if (consecutiveErrors >= 8) {
          clearInterval(workflowPollRef.current);
          workflowPollRef.current = null;
          setRunState("failed");
          appendAssistant({
            content: `\n\nWorkflow status polling stopped: failed to fetch task ${shortId(taskId)} after repeated errors.`,
          });
          clearWorkflowWatch();
          eventSourceRef.current?.close();
        }
        return;
      }
    }, 1800);
  }

  function clearWorkflowWatch() {
    if (workflowPollRef.current) {
      clearInterval(workflowPollRef.current);
      workflowPollRef.current = null;
    }
  }

  async function stopWorkflowRun() {
    const taskId = activeWorkflowTaskRef.current;
    if (!taskId) return;
    setRunState("stopping");
    try {
      await apiFetch(`/api/tasks/${encodeURIComponent(taskId)}/cancel`, {
        method: "POST",
      });
      setNotice({ kind: "ok", text: `Stopping task: ${shortId(taskId)}` });
      appendAssistant({ content: `\n\nStopping task ${shortId(taskId)}...` });
    } catch (error) {
      setNotice({ kind: "error", text: `Could not stop task: ${error.message}` });
      setRunState("running");
    }
  }

  function subscribeWorkflow(taskId) {
    const source = apiEventSource("/api/events");
    eventSourceRef.current = source;
    source.addEventListener("message", handleRunEvent);
    const eventNames = [
      "workflow_run_started",
      "workflow_agent_task_started",
      "workflow_agent_task_completed",
      "workflow_run_completed",
      "workflow_run_failed",
      "task_event",
      "tool_started",
      "tool_policy_decision",
      "tool_runtime_event",
      "tool_completed",
      "approval_required",
      "approval_resolved",
      "verification_report",
      "workflow_report_persisted",
      "workflow_report_persist_failed",
      "memory_postprocess_completed",
      "memory_postprocess_failed",
    ];
    for (const name of eventNames) source.addEventListener(name, handleRunEvent);
    source.onerror = () => {
      if (
        activeWorkflowTaskRef.current === taskId &&
        ["running", "waiting", "planning", "stopping"].includes(runStateRef.current)
      ) {
        setNotice({ kind: "error", text: "Event stream interrupted. Polling will continue until final status." });
      }
      source.close();
    };
  }

  function subscribeRun(runId) {
    const source = apiEventSource(`/api/chat/runs/${encodeURIComponent(runId)}/events`);
    eventSourceRef.current = source;
    source.addEventListener("message", handleRunEvent);
    const eventNames = [
      "run_started",
      "context_built",
      "graph_started",
      "tool_selection_started",
      "tool_selected",
      "model_started",
      "model_delta",
      "thinking_delta",
      "model_done",
      "tool_started",
      "tool_runtime_event",
      "tool_completed",
      "approval_required",
      "approval_resolved",
      "run_completed",
      "run_failed",
      "graph_completed",
    ];
    for (const name of eventNames) source.addEventListener(name, handleRunEvent);
    source.onerror = () => {
      if (runStateRef.current === "running") setRunState("idle");
      source.close();
    };
  }

  function handleRunEvent(rawEvent) {
    const envelope = eventData(rawEvent);
    if (!envelope) return;
    if (!eventBelongsToActiveWorkflow(envelope)) return;
    const { kind, data } = envelope;
    setEvents((prev) => [...prev.slice(-80), envelope]);

    if (kind === "workflow_run_started") {
      setRunState("running");
      setWorkflowPlan(data);
      setActiveNode(data.workflow?.start || "plan");
    } else if (kind === "workflow_agent_task_started") {
      if (data.child_task_id) workflowChildTaskIdsRef.current.add(data.child_task_id);
      appendTool({
        id: data.child_task_id,
        name: `${data.agent || "agent"} ${data.mode || ""}`.trim(),
        status: "running",
      });
    } else if (kind === "workflow_agent_task_completed") {
      appendTool({
        id: data.child_task_id,
        status: data.status === "completed" ? "done" : "error",
      });
    } else if (kind === "task_event") {
      handleTaskEvent(data.event);
    } else if (kind === "context_built") {
      if (!activeWorkflowTaskRef.current) {
        setWorkflowNodeState((prev) => ({ ...prev, context: "done", respond: "running" }));
        setActiveNode("respond");
      }
    } else if (kind === "model_delta") {
      appendAssistant({ content: data.delta || "" });
    } else if (kind === "thinking_delta") {
      appendAssistant({ thinking: data.delta || "" });
    } else if (kind === "tool_policy_decision") {
      const workflowNodeId = workflowNodeForToolCall(data.call_id);
      if (workflowNodeId) {
        appendTool({
          ...workflowNodePatch(workflowNodeId, data.name || data.tool_id),
          status: "approved",
        });
        return;
      }
      if (activeWorkflowTaskRef.current && data.call_id) {
        const nodeId = addDynamicWorkflowNode(data.call_id, data.tool_id, data.name);
        appendTool({
          ...workflowNodePatch(nodeId, data.name || data.tool_id),
          status: "approved",
        });
        return;
      }
      appendTool({
        id: data.call_id,
        name: friendlyToolLabel(data.tool_id, data.name),
        status: "approved",
      });
    } else if (kind === "tool_started") {
      const workflowNodeId = workflowNodeForToolCall(data.call_id, true);
      if (workflowNodeId) {
        appendTool({
          ...workflowNodePatch(workflowNodeId, data.name || data.tool_id),
          status: "running",
        });
        return;
      }
      if (activeWorkflowTaskRef.current && data.call_id) {
        const nodeId = addDynamicWorkflowNode(data.call_id, data.tool_id, data.name);
        setActiveNode(nodeId);
        activeNodeRef.current = nodeId;
        setWorkflowNodeState((prev) => ({ ...prev, [nodeId]: "running" }));
        appendTool({
          ...workflowNodePatch(nodeId, data.name || data.tool_id),
          status: "running",
        });
        return;
      }
      appendTool({
        id: data.call_id,
        name: data.name || data.tool_id,
        status: "running",
      });
    } else if (kind === "tool_runtime_event") {
      const workflowNodeId = workflowNodeForToolCall(data.call_id);
      if (workflowNodeId) {
        appendToolProgress(workflowNodeId, data.event);
        return;
      }
      appendToolProgress(data.call_id, data.event);
    } else if (kind === "tool_completed") {
      const workflowNodeId = workflowNodeForToolCall(data.call_id);
      if (workflowNodeId) {
        appendTool({
          ...workflowNodePatch(workflowNodeId, data.name || data.tool_id),
          ...workflowNodeEvidence(workflowNodeId, data),
          status: data.is_error ? "error" : "done",
        });
        return;
      }
      if (activeWorkflowTaskRef.current && data.call_id) {
        const nodeId = addDynamicWorkflowNode(data.call_id, data.tool_id, data.name);
        setWorkflowNodeState((prev) => ({ ...prev, [nodeId]: data.is_error ? "failure" : "success" }));
        appendTool({
          ...workflowNodePatch(nodeId, data.name || data.tool_id),
          ...workflowNodeEvidence(nodeId, data),
          status: data.is_error ? "error" : "done",
        });
        return;
      }
      appendTool({ id: data.call_id, status: data.is_error ? "error" : "done" });
    } else if (kind === "approval_required") {
      appendAssistant({
        approval: {
          id: data.approval_id,
          prompt: data.prompt,
          call: data.call,
        },
      });
      setRunState("waiting");
    } else if (kind === "approval_resolved") {
      settleApproval(data.approval_id, Boolean(data.approved));
      setRunState("running");
    } else if (kind === "run_completed") {
      setRunState("completed");
      if (activeWorkflowTaskRef.current) {
        setActiveNode("review");
      } else {
        setActiveNode("respond");
        setWorkflowNodeState((prev) => ({ ...prev, context: "done", respond: "done" }));
        fillEmptyAssistant("Run completed without a visible response.");
      }
      clearWorkflowWatch();
      eventSourceRef.current?.close();
      loadSessions();
    } else if (kind === "workflow_run_completed") {
      settleWorkflowSummary(data?.summary, "completed");
      clearWorkflowWatch();
      setRunState("completed");
      setActiveNode("end");
      const taskId = data.task_id || activeWorkflowTaskRef.current;
      setWorkflowTaskId(taskId || null);
      const finalContent = workflowFinalContent(data?.summary);
      appendAssistant({
        content: finalContent
          ? `\n\n${finalContent}\n\n---\nWorkflow completed. Task ${taskId} is saved in pwcli tasks.`
          : `\n\nWorkflow completed. Task ${taskId} is saved in pwcli tasks.`,
      });
      if (taskId) loadWorkflowMaterials(taskId);
      activeWorkflowTaskRef.current = null;
      loadSessions();
      eventSourceRef.current?.close();
    } else if (kind === "workflow_run_failed") {
      const summaryStatus = String(data?.summary?.status || "").toLowerCase();
      const shouldWait = summaryStatus === "interrupted";
      const reason = data?.error || data?.summary?.interrupt?.reason || "";
      const failedNode = failedWorkflowNode(data?.summary);
      settleWorkflowSummary(data?.summary, shouldWait ? "interrupted" : "failed");
      if (failedNode) {
        setActiveNode(failedNode);
        setWorkflowNodeState((prev) => ({ ...prev, [failedNode]: "failure" }));
        appendTool({ id: failedNode, status: "error" });
      }
      clearWorkflowWatch();
      setRunState("failed");
      appendAssistant({
        content: `\n\nWorkflow stopped: ${shouldWait ? "interrupted" : String(summaryStatus || "failed")}${reason ? ` - ${reason}` : ""}`,
      });
      activeWorkflowTaskRef.current = null;
      loadSessions();
      eventSourceRef.current?.close();
    } else if (
      kind === "memory_postprocess_completed" ||
      kind === "memory_postprocess_failed" ||
      kind === "workflow_report_persisted"
    ) {
      const taskId = data.task_id || activeWorkflowTaskRef.current || workflowTaskId;
      if (taskId) loadWorkflowMaterials(taskId);
    } else if (kind === "run_failed") {
      setRunState("failed");
      clearWorkflowWatch();
      if (!activeWorkflowTaskRef.current) {
        setWorkflowNodeState((prev) => ({ ...prev, respond: "error" }));
      }
      appendAssistant({ content: `\n\nRun failed: ${data.error || "unknown error"}` });
      activeWorkflowTaskRef.current = null;
      eventSourceRef.current?.close();
    } else if (kind === "model_started") {
      setActiveNode((prev) => prev || (activeWorkflowTaskRef.current ? "plan" : "respond"));
    } else if (kind === "tool_selected") {
      setActiveNode("execute");
    } else if (kind === "graph_completed") {
      setActiveNode(activeWorkflowTaskRef.current ? "review" : "respond");
    }
  }

  function eventBelongsToActiveWorkflow(envelope) {
    const taskId = activeWorkflowTaskRef.current;
    if (!taskId) return true;
    const data = envelope.data || {};
    const runtimeEvent = unwrapRuntimeEvent(data.event);
    return (
      envelope.task_id === taskId ||
      envelope.run_id === taskId ||
      data.task_id === taskId ||
      data.parent_task_id === taskId ||
      workflowChildTaskIdsRef.current.has(envelope.task_id) ||
      workflowChildTaskIdsRef.current.has(data.child_task_id) ||
      workflowChildTaskIdsRef.current.has(runtimeEvent?.task_id)
    );
  }

  function handleTaskEvent(rawRuntimeEvent) {
    const runtimeEvent = unwrapRuntimeEvent(rawRuntimeEvent);
    if (!runtimeEvent) return;
    if (runtimeEvent.type === "WorkflowNodeStarted") {
      setActiveNode(runtimeEvent.node_id);
      activeNodeRef.current = runtimeEvent.node_id;
      setWorkflowNodeState((prev) => ({ ...prev, [runtimeEvent.node_id]: "running" }));
      appendTool({
        ...workflowNodePatch(runtimeEvent.node_id, runtimeEvent.label),
        status: "running",
      });
    } else if (runtimeEvent.type === "WorkflowNodeCompleted") {
      setWorkflowNodeState((prev) => ({ ...prev, [runtimeEvent.node_id]: runtimeEvent.status }));
      appendTool({
        id: runtimeEvent.node_id,
        status: runtimeEvent.status === "success" ? "done" : "error",
      });
    } else if (runtimeEvent.type === "Output") {
      appendToolProgress(runtimeEvent.task_id, runtimeEvent);
    } else if (runtimeEvent.type === "Structured") {
      appendToolProgress(runtimeEvent.task_id, runtimeEvent.event);
    } else if (runtimeEvent.type === "Completed") {
      appendTool({ id: runtimeEvent.task_id, status: "done" });
    } else if (runtimeEvent.type === "Failed") {
      appendTool({ id: runtimeEvent.task_id, status: "error" });
      appendToolProgress(runtimeEvent.task_id, runtimeEvent.error || "failed");
    } else if (runtimeEvent.type === "VerificationRecorded") {
      appendTool({
        id: "verify",
        name: "Verification",
        status: runtimeEvent.passed ? "done" : "review",
      });
    }
  }

  function failedWorkflowNode(summary) {
    const outputs = summary?.outputs || {};
    const failed = Object.entries(outputs).find(([, output]) => output?.ok === false);
    if (failed?.[0]) return failed[0];
    const visited = Array.isArray(summary?.visited) ? summary.visited : [];
    return visited[visited.length - 1] || null;
  }

  function settleWorkflowSummary(summary, terminalStatus) {
    const visited = Array.isArray(summary?.visited) ? summary.visited.filter((id) => id !== "end") : [];
    if (visited.length === 0) return;
    const failedNode = terminalStatus === "completed" ? null : failedWorkflowNode(summary);
    const nextStates = {};
    for (const nodeId of visited) {
      const failed = nodeId === failedNode || summary?.outputs?.[nodeId]?.ok === false;
      nextStates[nodeId] = failed ? "failure" : "success";
    }
    setWorkflowNodeState((prev) => ({ ...prev, ...nextStates }));
    for (const [nodeId, status] of Object.entries(nextStates)) {
      appendTool({
        ...workflowNodePatch(nodeId),
        ...workflowNodeEvidence(nodeId, summary?.outputs?.[nodeId]),
        status: status === "success" ? "done" : "error",
      });
    }
  }

  function workflowNodeForToolCall(callId, create = false) {
    const taskId = activeWorkflowTaskRef.current;
    if (!taskId || !callId) return null;
    const existing = workflowToolCallNodeRef.current.get(callId);
    if (existing || !create) return existing || null;
    if (!String(callId).startsWith(`workflow-${taskId}-`)) return null;
    const nodeId = activeNodeRef.current;
    if (nodeId) workflowToolCallNodeRef.current.set(callId, nodeId);
    return nodeId || null;
  }

  function addDynamicWorkflowNode(callId, toolId, name) {
    const nodeId = dynamicNodeId(callId);
    const label = friendlyToolLabel(toolId, name);
    workflowToolCallNodeRef.current.set(callId, nodeId);
    setWorkflowPlan((prev) => {
      const current = prev || EMPTY_WORKFLOW;
      const workflow = current.workflow || EMPTY_WORKFLOW.workflow;
      const nodes = workflow.nodes || {};
      const next = {
        ...current,
        dynamic_order: Array.from(new Set([...(current.dynamic_order || []), nodeId])),
        workflow: {
          ...workflow,
          nodes: {
            ...nodes,
            [nodeId]: {
              id: nodeId,
              label,
              kind: {
                tool_call: {
                  tool_id: toolId || "tool",
                  arguments: {},
                },
              },
            },
          },
        },
      };
      workflowPlanRef.current = next;
      return next;
    });
    return nodeId;
  }

  function workflowNodePatch(nodeId, fallbackName) {
    const node = workflowPlanRef.current?.workflow?.nodes?.[nodeId];
    const kind = node?.kind || {};
    const toolCall = kind.tool_call;
    const modelTurn = kind.model_turn;
    const details = [];
    let summary = "";
    if (toolCall) {
      const args = toolCall.arguments || {};
      const action = args.action || "call";
      const query = args.query || "";
      const maxResults = args.max_results || "";
      summary = query
        ? `${toolCall.tool_id} · ${action} "${clipText(query, 96)}"`
        : `${toolCall.tool_id} · ${action}`;
      if (query) details.push(`Query: ${query}`);
      if (maxResults) details.push(`Requested results: ${maxResults}`);
    } else if (modelTurn?.prompt) {
      summary = `Model step · ${clipText(modelTurn.prompt, 120)}`;
      details.push(`Prompt: ${modelTurn.prompt}`);
    }
    return {
      id: nodeId,
      name: nodeLabel(nodeId, node) || fallbackName,
      summary,
      details,
    };
  }

  function workflowNodeEvidence(nodeId, output) {
    const content = textOutput(output);
    const toolId = output?.tool_id || workflowPlanRef.current?.workflow?.nodes?.[nodeId]?.kind?.tool_call?.tool_id || "";
    if (toolId === "builtin.anysearch") {
      const parsed = parseSearchResultContent(content);
      const details = [];
      const planned = workflowNodePatch(nodeId).details || [];
      details.push(...planned);
      details.push("Read mode: search results/snippets only; no full-paper extract/read-paper step was run.");
      for (const result of parsed.results.slice(0, 4)) {
        details.push(`${result.index}. ${result.title}${result.url ? ` · ${result.url}` : ""}`);
      }
      return {
        summary: `Searched web · ${parsed.countLabel || `${parsed.results.length} visible results`}`,
        details,
      };
    }
    if (toolId === "builtin.local_file_index") {
      const matches = Array.isArray(output?.metadata?.matches) ? output.metadata.matches : [];
      const details = [...(workflowNodePatch(nodeId).details || [])];
      if (matches.length === 0) details.push("No local files matched this query.");
      for (const match of matches.slice(0, 4)) {
        details.push(`${match.path || "file"}${match.score != null ? ` · score ${match.score}` : ""}`);
      }
      return {
        summary: matches.length ? `Searched local context · ${matches.length} matches` : "Searched local context · no matches",
        details,
      };
    }
    if (nodeId === "read_papers") {
      const papers = Array.isArray(output?.papers) ? output.papers : [];
      const details = [];
      for (const paper of papers) {
        const level = paper.read_level || "unknown";
        const title = paper.title || paper.url || "paper";
        details.push(`${level}: ${title}${paper.pdf_url ? ` · ${paper.pdf_url}` : ""}`);
        if (paper.error) details.push(`Error: ${clipText(paper.error, 220)}`);
      }
      if (papers.length === 0) details.push("No arXiv/PDF paper candidates were found in the search results.");
      return {
        summary: `Read papers · ${papers.length} candidate${papers.length === 1 ? "" : "s"} · ${output?.read_level || "unknown"}`,
        details,
      };
    }
    if (nodeId === "adaptive_research") {
      const details = [];
      const toolResults = Array.isArray(output?.tool_results) ? output.tool_results : [];
      details.push(`Rounds: ${output?.round_count || "unknown"}`);
      details.push(`Tool results: ${output?.tool_result_count ?? toolResults.length}`);
      for (const result of toolResults.slice(0, 5)) {
        const label = result.is_error ? "error" : "ok";
        details.push(`${label}: ${clipText(result.content || "", 220)}`);
      }
      if (content) details.push(`Final adaptive output: ${clipText(content, 260)}`);
      return {
        summary: `Adaptive execution · ${output?.round_count || "?"} rounds · ${output?.tool_result_count ?? toolResults.length} tool result${(output?.tool_result_count ?? toolResults.length) === 1 ? "" : "s"}`,
        details,
      };
    }
    if (content) {
      return {
        summary: `${nodeLabel(nodeId, workflowPlanRef.current?.workflow?.nodes?.[nodeId])} produced ${content.length.toLocaleString()} chars`,
        details: [clipText(content, 260)],
      };
    }
    return {};
  }

  function parseSearchResultContent(content) {
    const count = content.match(/Search Results \(([^)]+)\)/i)?.[1] || "";
    const lines = String(content || "").split(/\n/);
    const results = [];
    for (let index = 0; index < lines.length; index += 1) {
      const title = lines[index].match(/^###\s+(\d+)\.\s+(.+)/);
      if (!title) continue;
      let url = "";
      for (let offset = index + 1; offset < Math.min(lines.length, index + 5); offset += 1) {
        const match = lines[offset].match(/-\s+\*\*URL\*\*:\s+(.+)/);
        if (match) {
          url = match[1].trim();
          break;
        }
      }
      results.push({ index: title[1], title: title[2].trim(), url });
    }
    return { countLabel: count, results };
  }

  function workflowFinalContent(summary) {
    const outputs = summary?.outputs || {};
    const finalReport = textOutput(outputs.final_report);
    if (finalReport) return finalReport;
    const adaptive = textOutput(outputs.adaptive_research);
    if (adaptive) return adaptive;
    const synthesize = textOutput(outputs.synthesize);
    const verification = textOutput(outputs.verify_sources);
    if (synthesize && verification) {
      return `## Research synthesis\n\n${synthesize}\n\n## Verification notes\n\n${verification}`;
    }
    const visited = Array.isArray(summary?.visited) ? [...summary.visited].reverse() : [];
    for (const nodeId of visited) {
      if (nodeId === "end") continue;
      const content = textOutput(outputs[nodeId]);
      if (content) return content;
    }
    return "";
  }

  async function loadWorkflowMaterials(taskId = workflowTaskId) {
    if (!taskId) return;
    setWorkflowMaterialsLoading(true);
    try {
      const data = await apiFetch(`/api/tasks/${encodeURIComponent(taskId)}/materials`);
      setWorkflowMaterials(data);
    } catch (error) {
      setNotice({ kind: "error", text: `Could not load materials: ${error.message}` });
    } finally {
      setWorkflowMaterialsLoading(false);
    }
  }

  async function openMaterialArtifact(artifactId) {
    if (!artifactId) return;
    setOpenMaterial((prev) => (prev?.artifact_id === artifactId ? null : { artifact_id: artifactId, loading: true }));
    try {
      const data = await apiFetch(`/api/materials/${encodeURIComponent(artifactId)}`);
      setOpenMaterial(data);
    } catch (error) {
      setOpenMaterial({ artifact_id: artifactId, error: error.message });
    }
  }

  async function postprocessWorkflowMemory(taskId = workflowTaskId) {
    if (!taskId) return;
    setWorkflowMaterialsLoading(true);
    try {
      await apiFetch(`/api/memory/postprocess/task/${encodeURIComponent(taskId)}`, { method: "POST" });
      await loadWorkflowMaterials(taskId);
      setNotice({ kind: "ok", text: "Memory extraction updated" });
    } catch (error) {
      setNotice({ kind: "error", text: `Memory extraction failed: ${error.message}` });
    } finally {
      setWorkflowMaterialsLoading(false);
    }
  }

  function textOutput(output) {
    if (!output || typeof output !== "object") return "";
    return typeof output.content === "string" ? output.content.trim() : "";
  }

  function canStopWorkflow() {
    return (
      activeWorkflowTaskRef.current &&
      ["planning", "running", "waiting", "stopping"].includes(runStateRef.current)
    );
  }

  function appendAssistant(patch) {
    setMessages((prev) => {
      const next = [...prev];
      let index = next.length - 1;
      while (index >= 0 && next[index].role !== "assistant") index -= 1;
      if (index < 0) return prev;
      const current = next[index];
      next[index] = {
        ...current,
        content: current.content + (patch.content || ""),
        thinking: (current.thinking || "") + (patch.thinking || ""),
        approval: patch.approval || current.approval,
      };
      return next;
    });
  }

  function fillEmptyAssistant(content) {
    setMessages((prev) => {
      const next = [...prev];
      let index = next.length - 1;
      while (index >= 0 && next[index].role !== "assistant") index -= 1;
      if (index < 0) return prev;
      const current = next[index];
      if ((current.content || "").trim() || (current.thinking || "").trim()) return prev;
      next[index] = { ...current, content };
      return next;
    });
  }

  function appendTool(toolPatch) {
    if (!toolPatch?.id) return;
    setMessages((prev) => {
      const next = [...prev];
      let index = next.length - 1;
      while (index >= 0 && next[index].role !== "assistant") index -= 1;
      if (index < 0) return prev;
      const current = next[index];
      const tools = current.tools || [];
      const existing = tools.findIndex((tool) => tool.id === toolPatch.id);
      const nextTools =
        existing >= 0
          ? tools.map((tool, idx) => (idx === existing ? { ...tool, ...toolPatch } : tool))
          : [...tools, { status: "running", progress: [], ...toolPatch }];
      next[index] = { ...current, tools: nextTools };
      return next;
    });
  }

  function appendToolProgress(callId, runtimeEvent) {
    if (!callId) return;
    const line = eventLine(runtimeEvent);
    if (!line) return;
    setMessages((prev) => {
      const next = [...prev];
      let index = next.length - 1;
      while (index >= 0 && next[index].role !== "assistant") index -= 1;
      if (index < 0) return prev;
      const current = next[index];
      next[index] = {
        ...current,
        tools: (current.tools || []).map((tool) =>
          tool.id === callId
            ? { ...tool, progress: [...(tool.progress || []), line].slice(-5) }
            : tool,
        ),
      };
      return next;
    });
  }

  function settleApproval(approvalId, approved) {
    setMessages((prev) =>
      prev.map((message) => {
        if (message.approval?.id !== approvalId) return message;
        return {
          ...message,
          approval: {
            ...message.approval,
            status: approved ? "approved" : "rejected",
          },
        };
      }),
    );
  }

  async function resolveApproval(approvalId, approved) {
    try {
      await apiFetch(`/api/approvals/${encodeURIComponent(approvalId)}`, {
        method: "POST",
        body: JSON.stringify({ approved }),
      });
      settleApproval(approvalId, approved);
      setRunState("running");
    } catch (error) {
      setNotice({ kind: "error", text: `Could not resolve approval: ${error.message}` });
    }
  }

  async function openSession(entry) {
    setActiveSession(entry);
    setActiveNav("workbench");
    setSessionLoadingId(entry.id);
    setMessages([
      { id: `session-loading-${entry.id}`, role: "assistant", content: "Loading full conversation...", thinking: "", tools: [] },
    ]);
    try {
      const data = await apiFetch(`/api/sessions/${encodeURIComponent(entry.id)}`);
      const fullMessages = Array.isArray(data.messages) ? data.messages : [];
      const firstUser = fullMessages.find((message) => message.role === "user")?.content;
      setActiveSession({ ...(data.entry || entry), user_preview: firstUser || entry.user_preview });
      setMessages(
        fullMessages.length
          ? fullMessages.map((message, index) => ({
              id: message.id || `history-${entry.id}-${index}`,
              role: message.role || "assistant",
              content: message.content || "",
              thinking: "",
              tools: [],
              tool_call_id: message.tool_call_id,
              is_error: message.is_error,
            }))
          : [
              { id: `session-user-${entry.id}`, role: "user", content: entry.user_preview || "Previous thread" },
              {
                id: `session-assistant-${entry.id}`,
                role: "assistant",
                content: entry.assistant_preview || "Open this thread by continuing the conversation.",
                thinking: "",
                tools: [],
              },
            ],
      );
    } catch (error) {
      setMessages([
        { id: `session-user-${entry.id}`, role: "user", content: entry.user_preview || "Previous thread" },
        {
          id: `session-assistant-${entry.id}`,
          role: "assistant",
          content: entry.assistant_preview || "Open this thread by continuing the conversation.",
          thinking: "",
          tools: [],
        },
      ]);
      setNotice({ kind: "error", text: `Could not load full chat: ${error.message}` });
    } finally {
      setSessionLoadingId(null);
    }
  }

  const activeNavLabel = NAV_ITEMS.find((item) => item.id === activeNav)?.label || "Workbench";
  const topbarTitle =
    activeNav === "workbench"
      ? activeSession
        ? compactThreadTitle(activeSession)
        : "Implement pwcli web entry"
      : activeNavLabel;
  const isWorkflowRunActive = Boolean(activeWorkflowTaskRef.current);
  const showChatManagement = activeNav === "workbench" && historyOpen;

  return (
    <div
      className={`app-shell ${showChatManagement ? "" : "history-closed"}`}
      style={{ "--history-width": `${historyWidth}px` }}
    >
      <IconRail
        activeNav={activeNav}
        setActiveNav={setActiveNav}
        historyOpen={historyOpen}
        setHistoryOpen={setHistoryOpen}
      />
      {showChatManagement && (
        <HistorySidebar
          folders={folders}
          folderOpen={folderOpen}
          setFolderOpen={setFolderOpen}
          sessionsByFolder={sessionsByFolder}
          sessions={filteredSessions}
          historyTab={historyTab}
          setHistoryTab={setHistoryTab}
          search={search}
          setSearch={setSearch}
          addFolder={addFolder}
          activeSession={activeSession}
          openSession={openSession}
          assignSession={assignSession}
          deleteSession={deleteSession}
          draggingSessionId={draggingSessionId}
          setDraggingSessionId={setDraggingSessionId}
          onResize={startResize}
          onCollapse={() => setHistoryOpen(false)}
        />
      )}
      <main className="chat-main">
        <header className="topbar">
          <div>
            <div className="thread-kicker">{activeNavLabel}</div>
            <h1>{topbarTitle}</h1>
          </div>
        </header>

        {notice && <NoticeBar notice={notice} clear={() => setNotice(null)} />}

        {activeNav === "workbench" ? (
          <>
            <section className="conversation">
              <WorkflowStrip
                plan={workflowPlan}
                nodes={workflowNodes}
                activeNode={activeNode}
                nodeStates={workflowNodeState}
                runState={runState}
              />
              <div className="message-stack">
                {messages.map((message) => (
                  <Message
                    key={message.id}
                    message={message}
                    thinkingOpen={thinkingOpen}
                    setThinkingOpen={setThinkingOpen}
                    resolveApproval={resolveApproval}
                  />
                ))}
                {sessionLoadingId && <StatusLine label="Loading chat" />}
                {runState === "planning" && <StatusLine label="Planning route" />}
                {runState === "running" && (
                  <StatusLine label={isWorkflowRunActive ? "Running graph" : "Thinking"} />
                )}
                {runState === "waiting" && <StatusLine label="Waiting for approval" />}
                {runState === "stopping" && (
                  <StatusLine label={isWorkflowRunActive ? "Stopping workflow" : "Stopping"} />
                )}
                {(workflowMaterials || workflowTaskId) && (
                  <MaterialsPanel
                    taskId={workflowTaskId}
                    data={workflowMaterials}
                    loading={workflowMaterialsLoading}
                    openMaterial={openMaterial}
                    openArtifact={openMaterialArtifact}
                    refresh={loadWorkflowMaterials}
                    postprocess={postprocessWorkflowMemory}
                  />
                )}
                <div ref={messagesEndRef} />
              </div>
            </section>

            <Composer
              value={prompt}
              setValue={setPrompt}
              send={sendPrompt}
              disabled={runState === "running" || runState === "planning" || runState === "waiting" || runState === "stopping"}
              focused={composerFocused}
              setFocused={setComposerFocused}
              workflowKind={workflowKindLabel(workflowKind)}
              workflowKinds={WORKFLOW_KIND_OPTIONS}
              onWorkflowKindChange={setWorkflowKind}
              status={status}
              showThinking={showThinking}
              toggleShowThinking={toggleShowThinking}
              canStop={canStopWorkflow()}
              stop={stopWorkflowRun}
            />
          </>
        ) : activeNav === "tasks" ? (
          <TasksView
            data={tasksData}
            refresh={loadTasks}
            deleteTask={deleteTask}
            createTask={createTask}
            createTaskAndDecompose={createTaskAndDecompose}
            decomposeTask={decomposeTask}
            updateTask={updateTask}
            cycleTaskStatus={cycleTaskStatus}
            decomposeBusy={taskDecomposeBusy}
            createBusy={taskCreateSubmitting}
          />
        ) : activeNav === "memory" ? (
          <MemoryView data={memoryData} refresh={loadMemory} />
        ) : (
          <SettingsView
            status={status}
            draft={settingsDraft}
            setDraft={setSettingsDraft}
            save={saveSettingsDraft}
            saving={settingsSaving}
            refresh={loadStatus}
          />
        )}
      </main>
    </div>
  );
}

function NoticeBar({ notice, clear }) {
  return (
    <div className={`notice-bar ${notice.kind === "error" ? "error" : ""}`}>
      <span>{notice.text}</span>
      <button onClick={clear}>Dismiss</button>
    </div>
  );
}

function TasksView({
  data,
  refresh,
  deleteTask,
  createTask,
  createTaskAndDecompose,
  decomposeTask,
  updateTask,
  cycleTaskStatus,
  decomposeBusy,
  createBusy,
}) {
  const tasks = normalizePersonalTaskData(data).tasks;
  const [titleDraft, setTitleDraft] = useState("");
  const [collapsedGroups, setCollapsedGroups] = useState(() => new Set(TASK_GROUPS.filter((group) => !group.defaultOpen).map((group) => group.key)));
  const [selectedTaskId, setSelectedTaskId] = useState(null);
  const groups = groupPersonalTasks(tasks);
  const selectedTask = selectedTaskId ? tasks.find((task) => task.id === selectedTaskId) || null : null;
  const todayTasks = tasks.filter((task) => taskGroupKey(task) === "today" || (taskDone(task) && task.completedAt && new Date(task.completedAt).toDateString() === new Date().toDateString()));
  const todayDone = todayTasks.filter(taskDone).length;
  const weekCount = tasks.filter((task) => ["overdue", "today", "thisweek"].includes(taskGroupKey(task))).length;

  async function handleCreate(event) {
    event.preventDefault();
    const success = await createTask(titleDraft);
    if (success) setTitleDraft("");
  }

  async function handleCreateAndDecompose(event) {
    event.preventDefault();
    const success = await createTaskAndDecompose(titleDraft);
    if (success) setTitleDraft("");
  }

  function toggleGroup(key) {
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  return (
    <section className="workspace-view tasks-workspace">
      <div className="task-hero">
        <div className="task-progress">
          <strong>{todayTasks.length ? Math.round((todayDone / todayTasks.length) * 100) : 0}%</strong>
          <span>Today</span>
        </div>
        <div className="task-hero-metrics">
          <span>{todayDone}/{todayTasks.length} done</span>
          <span>{weekCount} this week</span>
        </div>
        <form className="task-create" onSubmit={handleCreate}>
          <Plus size={16} />
          <input
            value={titleDraft}
            onChange={(event) => setTitleDraft(event.target.value)}
            placeholder="添加任务，支持 @今天 #高"
            aria-label="Task title"
          />
          <div className="task-create-actions">
            <button type="submit" className="row-action" disabled={!titleDraft.trim() || createBusy}>
              {createBusy ? <Loader2 size={14} className="spin" /> : "Create"}
            </button>
            <button
              type="button"
              className="row-action"
              disabled={!titleDraft.trim() || createBusy}
              onClick={handleCreateAndDecompose}
            >
              拆解
            </button>
          </div>
        </form>
        <button className="ghost-button" onClick={refresh} title="Refresh local tasks">
          <RefreshCw size={15} />
        </button>
      </div>

      {groups.length === 0 ? (
        <div className="empty-state">
          <SquareCheckBig size={18} />
          <span>今天还没有任务</span>
        </div>
      ) : (
        <div className="task-groups">
          {groups.map((group) => {
            const open = !collapsedGroups.has(group.key);
            return (
              <section className="task-group" key={group.key}>
                <button className="task-group-head" onClick={() => toggleGroup(group.key)}>
                  <span>{group.title}</span>
                  <small>{group.tasks.length}</small>
                  <ChevronDown size={15} className={open ? "" : "rotate"} />
                </button>
                {open && (
                  <div className="task-list">
                    {group.tasks.map((task) => (
                      <PersonalTaskRow
                        key={task.id}
                        task={task}
                        selected={selectedTaskId === task.id}
                        busy={Boolean(decomposeBusy?.[task.id])}
                        onSelect={() => setSelectedTaskId(task.id)}
                        onCycle={() => cycleTaskStatus(task.id)}
                        onDelete={() => deleteTask(task.id, task.title)}
                        onDecompose={() => decomposeTask(task.id, { goal: task.title })}
                      />
                    ))}
                  </div>
                )}
              </section>
            );
          })}
        </div>
      )}
      <PersonalTaskDrawer
        task={selectedTask}
        onClose={() => setSelectedTaskId(null)}
        onUpdate={updateTask}
      />
    </section>
  );
}

function PersonalTaskRow({ task, selected, busy, onSelect, onCycle, onDelete, onDecompose }) {
  const subDone = task.subTasks.filter((subTask) => subTask.completed).length;
  const dueLabel = relativeTaskDateLabel(task.dueDate);
  return (
    <article className={`personal-task-row ${selected ? "selected" : ""} ${taskDone(task) ? "done" : ""}`} onClick={onSelect}>
      <button className={`task-status-dot ${task.status}`} onClick={(event) => { event.stopPropagation(); onCycle(); }} title="Cycle status">
        {task.status === "done" ? <Check size={14} /> : task.status === "in_progress" ? <Loader2 size={14} className="spin" /> : <Circle size={12} />}
      </button>
      <span className={`priority-bar ${task.priority}`} />
      <div className="personal-task-main">
        <strong>{task.title}</strong>
        <div className="task-chip-row">
          {dueLabel && <span><CalendarDays size={11} />{dueLabel}</span>}
          {task.scheduledStart && <span><Clock3 size={11} />{task.scheduledStart}{task.scheduledEnd ? `-${task.scheduledEnd}` : ""}</span>}
          {task.subTasks.length > 0 && <span>{subDone}/{task.subTasks.length} 步</span>}
        </div>
      </div>
      <span className={`status-pill ${statusClass(task.status)}`}>{task.status}</span>
      <div className="task-row-actions">
        <button className="row-action" disabled={busy} onClick={(event) => { event.stopPropagation(); onDecompose(); }}>
          {busy ? <Loader2 size={14} className="spin" /> : "拆解"}
        </button>
        <button className="row-action row-delete" onClick={(event) => { event.stopPropagation(); onDelete(); }} title="Delete task">
          <Trash2 size={14} />
        </button>
      </div>
    </article>
  );
}

function PersonalTaskDrawer({ task, onClose, onUpdate }) {
  const [draft, setDraft] = useState(task || null);

  useEffect(() => {
    setDraft(task);
  }, [task?.id]);

  useEffect(() => {
    if (!task) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [task, onClose]);

  if (!task || !draft) return null;

  function patch(nextPatch) {
    const next = { ...draft, ...nextPatch };
    setDraft(next);
    onUpdate(task.id, nextPatch);
  }

  function patchSubTask(subTaskId, nextPatch) {
    const subTasks = draft.subTasks.map((subTask) =>
      subTask.id === subTaskId ? { ...subTask, ...nextPatch } : subTask,
    );
    patch({ subTasks });
  }

  function addSubTask() {
    patch({
      subTasks: [
        ...draft.subTasks,
        { id: makePersonalTaskId(), title: "", completed: false, dueDate: "", scheduledStart: "", scheduledEnd: "" },
      ],
    });
  }

  function removeSubTask(subTaskId) {
    patch({ subTasks: draft.subTasks.filter((subTask) => subTask.id !== subTaskId) });
  }

  return (
    <div className="task-drawer-layer">
      <button className="task-drawer-scrim" onClick={onClose} aria-label="Close task drawer" />
      <aside className="task-drawer">
        <header>
          <input value={draft.title} onChange={(event) => patch({ title: event.target.value })} aria-label="Task title" />
          <button onClick={onClose} title="Close"><X size={17} /></button>
        </header>
        <div className="task-drawer-body">
          <label>
            <span>Priority</span>
            <select value={draft.priority} onChange={(event) => patch({ priority: event.target.value })}>
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
            </select>
          </label>
          <label>
            <span>Due date</span>
            <input type="date" value={draft.dueDate || ""} onChange={(event) => patch({ dueDate: event.target.value })} />
          </label>
          <div className="drawer-field-grid">
            <label>
              <span>Start</span>
              <input type="time" value={draft.scheduledStart || ""} onChange={(event) => patch({ scheduledStart: event.target.value })} />
            </label>
            <label>
              <span>End</span>
              <input type="time" value={draft.scheduledEnd || ""} onChange={(event) => patch({ scheduledEnd: event.target.value })} />
            </label>
          </div>
          <label>
            <span>Notes</span>
            <textarea value={draft.notes || ""} onChange={(event) => patch({ notes: event.target.value })} rows={5} />
          </label>
          <section className="drawer-subtasks">
            <div>
              <span>Subtasks</span>
              <button className="drawer-add-button" onClick={addSubTask} title="Add subtask" aria-label="Add subtask">
                <Plus size={15} />
              </button>
            </div>
            {draft.subTasks.length === 0 ? (
              <p>No subtasks yet</p>
            ) : (
              draft.subTasks.map((subTask) => (
                <div className="drawer-subtask" key={subTask.id}>
                  <button onClick={() => patchSubTask(subTask.id, { completed: !subTask.completed })} title="Toggle subtask">
                    {subTask.completed ? <Check size={13} /> : <Circle size={11} />}
                  </button>
                  <input value={subTask.title} onChange={(event) => patchSubTask(subTask.id, { title: event.target.value })} />
                  <button onClick={() => removeSubTask(subTask.id)} title="Remove subtask"><Trash2 size={13} /></button>
                </div>
              ))
            )}
          </section>
        </div>
      </aside>
    </div>
  );
}

function MemoryView({ data, refresh }) {
  const graph = data.graph || EMPTY_MEMORY.graph;
  const facts = data.facts || [];
  const inferences = data.inferences || [];
  const hypotheses = data.hypotheses || [];
  const candidates = data.candidates || [];
  return (
    <section className="workspace-view memory-view">
      <div className="workspace-head">
        <div>
          <span>Memory graph</span>
          <strong>{graph.facts || 0}</strong>
        </div>
        <button className="ghost-button" onClick={refresh}>
          <RefreshCw size={15} />
          <span>Refresh</span>
        </button>
      </div>
      <div className="memory-layout">
        <MemoryOrbit graph={graph} facts={facts} />
        <div className="memory-layers">
          <LayerMetric icon={Database} label="Facts" value={facts.length} />
          <LayerMetric icon={GitBranch} label="Inferences" value={inferences.length} />
          <LayerMetric icon={Sparkles} label="Hypotheses" value={hypotheses.length} />
          <LayerMetric icon={Archive} label="Inbox" value={candidates.length} />
        </div>
      </div>
      <div className="data-list memory-list">
        {facts.length === 0 ? (
          <div className="empty-state">
            <MemoryStick size={18} />
            <span>No accepted memory yet</span>
          </div>
        ) : (
          facts.slice(0, 10).map((fact) => (
            <div className="data-row" key={fact.id}>
              <div className="row-icon">
                <Database size={15} />
              </div>
              <div className="row-main">
                <strong>{fact.statement}</strong>
                <span>{fact.source || shortId(fact.id)}</span>
              </div>
              <span className={`status-pill ${statusClass(fact.status)}`}>{fact.status || "active"}</span>
            </div>
          ))
        )}
      </div>
      {(inferences.length > 0 || hypotheses.length > 0) && (
        <div className="layer-preview-grid">
          {inferences.length > 0 && (
            <div className="inference-chain">
              {inferences.slice(0, 5).map((item) => (
                <div className="inference-link" key={item.id}>
                  <GitBranch size={14} />
                  <span>{item.statement}</span>
                </div>
              ))}
            </div>
          )}
          {hypotheses.length > 0 && (
            <div className="hypothesis-cloud">
              {hypotheses.slice(0, 8).map((item) => (
                <span key={item.id} className="hypothesis-pill">
                  {item.statement}
                  <small>{Math.round((item.confidence || 0) * 100)}%</small>
                </span>
              ))}
            </div>
          )}
        </div>
      )}
    </section>
  );
}

function MemoryOrbit({ graph, facts }) {
  const count = Math.min(Math.max(facts.length, graph.facts || 0), 12);
  const nodes = Array.from({ length: count }, (_, index) => index);
  return (
    <div className="memory-orbit" aria-label="Memory fact graph">
      <div className="orbit-core">
        <MemoryStick size={22} />
        <strong>{graph.facts || 0}</strong>
      </div>
      <div className="orbit-ring ring-a" />
      <div className="orbit-ring ring-b" />
      {nodes.map((node) => {
        const angle = (360 / Math.max(nodes.length, 1)) * node;
        const radius = node % 2 === 0 ? 96 : 126;
        return (
          <span
            key={node}
            className="orbit-node"
            style={{
              "--x": `${Math.cos((angle * Math.PI) / 180) * radius}px`,
              "--y": `${Math.sin((angle * Math.PI) / 180) * radius * 0.56}px`,
            }}
          />
        );
      })}
    </div>
  );
}

function LayerMetric({ icon: Icon, label, value, muted }) {
  return (
    <div className="layer-metric">
      <Icon size={16} />
      <span>{label}</span>
      <strong>{value}</strong>
      {muted && <small>{muted}</small>}
    </div>
  );
}

function GlassSelect({ value, options, onChange, disabled = false, placeholder = "Select" }) {
  const [open, setOpen] = useState(false);
  const ref = useRef(null);
  const selected = options.find((option) => option.value === value);

  useEffect(() => {
    if (!open) return undefined;
    const onPointerDown = (event) => {
      if (!ref.current?.contains(event.target)) setOpen(false);
    };
    const onKeyDown = (event) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  function choose(nextValue) {
    onChange(nextValue);
    setOpen(false);
  }

  return (
    <div className={`glass-select ${open ? "open" : ""}`} ref={ref}>
      <button
        type="button"
        className="glass-select-trigger"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((next) => !next)}
      >
        <span>{selected?.label || value || placeholder}</span>
        <ChevronDown size={17} className={open ? "rotate" : ""} />
      </button>
      {open && !disabled && (
        <div className="glass-select-menu" role="listbox">
          {options.map((option) => (
            <button
              type="button"
              key={option.value}
              className={`glass-select-option ${option.value === value ? "selected" : ""}`}
              role="option"
              aria-selected={option.value === value}
              onClick={() => choose(option.value)}
            >
              <Check size={15} />
              <span>{option.label}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function SecretInput({ value, onChange, placeholder = "" }) {
  const [visible, setVisible] = useState(false);
  return (
    <div className="secret-input">
      <input
        type={visible ? "text" : "password"}
        value={value || ""}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder}
        autoComplete="off"
        spellCheck="false"
      />
      <button
        type="button"
        className="secret-toggle"
        title={visible ? "Hide API key" : "Show API key"}
        aria-label={visible ? "Hide API key" : "Show API key"}
        onClick={() => setVisible((next) => !next)}
      >
        {visible ? <EyeOff size={15} /> : <Eye size={15} />}
      </button>
    </div>
  );
}

const AGENT_IDS = ["codex", "claude", "agy", "qodercli"];
const ROUTE_IDS = ["code", "research", "ops", "general"];
const MODE_IDS = ["plan", "execute", "review"];
const PROVIDER_PROTOCOL_OPTIONS = ["openai", "anthropic", "nvidia"].map((value) => ({ value, label: value }));
const PROVIDER_API_OPTIONS = ["chat_completions", "responses"].map((value) => ({ value, label: value }));
const AGENT_EFFORT_VALUES = {
  codex: ["low", "medium", "high", "xhigh"],
  claude: ["low", "medium", "high", "xhigh", "max"],
  qodercli: ["low", "medium", "high", "xhigh"],
  agy: [],
};
const RISK_LEVEL_OPTIONS = ["safe", "low", "medium", "high", "destructive"].map((value) => ({ value, label: value }));
const APPROVAL_MODE_OPTIONS = ["policy", "always", "never", "deny"].map((value) => ({ value, label: value }));
const NETWORK_POLICY_OPTIONS = ["allow", "deny", "local_only"].map((value) => ({ value, label: value }));
const MCP_TRANSPORT_OPTIONS = ["stdio", "http", "sse"].map((value) => ({ value, label: value }));
const MEMORY_DOWNLOAD_OPTIONS = ["ask", "auto", "never"].map((value) => ({ value, label: value }));
const MEMORY_EMBEDDING_MODEL_OPTIONS = ["bge-small-zh-v1.5", "bge-large-zh-v1.5", "bge-m3"].map((value) => ({ value, label: value }));

function parsePositiveInt(value, fallback = 0) {
  const parsed = parseInt(value, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function csvToList(value) {
  return String(value || "")
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function listToCsv(value) {
  return Array.isArray(value) ? value.join(", ") : "";
}

function agentEffortOptions(agent, includeInherit = false) {
  const values = AGENT_EFFORT_VALUES[agent] || ["low", "medium", "high", "xhigh"];
  const options = values.map((value) => ({ value, label: value }));
  return includeInherit ? [{ value: "", label: "Use profile" }, ...options] : options;
}

function normalizeAgentEffortValue(agent, value, fallback = "high") {
  const values = AGENT_EFFORT_VALUES[agent] || ["low", "medium", "high", "xhigh"];
  if (values.includes(value)) return value;
  if (value === "max" && values.includes("xhigh")) return "xhigh";
  return values.includes(fallback) ? fallback : "";
}

function agentModelOptions(config, agent, currentValue, inheritLabel = "agent default") {
  const rawOptions = config?.agent_model_options?.[agent] || [];
  const normalized = rawOptions
    .map((option) => {
      if (typeof option === "string") return { value: option, label: option };
      return { value: option?.value || option?.label || "", label: option?.label || option?.value || "" };
    })
    .filter((option) => option.value);
  const seen = new Set(normalized.map((option) => option.value));
  const options = [{ value: "", label: inheritLabel }, ...normalized];
  if (currentValue && !seen.has(currentValue)) {
    options.push({ value: currentValue, label: currentValue });
  }
  return options;
}

function JsonEditor({ value, onChange, rows = 4 }) {
  const [text, setText] = useState(() => JSON.stringify(value ?? {}, null, 2));
  useEffect(() => {
    setText(JSON.stringify(value ?? {}, null, 2));
  }, [value]);
  return (
    <textarea
      className="json-editor"
      rows={rows}
      value={text}
      onChange={(event) => setText(event.target.value)}
      onBlur={() => {
        try {
          onChange(JSON.parse(text || "{}"));
        } catch {
          setText(JSON.stringify(value ?? {}, null, 2));
        }
      }}
    />
  );
}

function SettingsSection({ title, children, action }) {
  return (
    <section className="settings-section">
      <header>
        <h2>{title}</h2>
        {action}
      </header>
      <div className="settings-grid">{children}</div>
    </section>
  );
}

function KeyValueSelectMap({ value, onChange, valueOptions, addLabel = "Add" }) {
  const entries = Object.entries(value || {});
  function setEntry(index, nextKey, nextValue) {
    const next = {};
    entries.forEach(([key, itemValue], idx) => {
      const finalKey = idx === index ? nextKey : key;
      const finalValue = idx === index ? nextValue : itemValue;
      if (finalKey.trim()) next[finalKey.trim()] = finalValue;
    });
    onChange(next);
  }
  function removeEntry(index) {
    const next = {};
    entries.forEach(([key, itemValue], idx) => {
      if (idx !== index && key.trim()) next[key] = itemValue;
    });
    onChange(next);
  }
  return (
    <div className="kv-select-map">
      {entries.map(([key, itemValue], index) => (
        <div className="kv-select-row" key={`${key}-${index}`}>
          <input value={key} placeholder="tool id" onChange={(event) => setEntry(index, event.target.value, itemValue)} />
          <GlassSelect value={itemValue || valueOptions[0]?.value || ""} options={valueOptions} onChange={(nextValue) => setEntry(index, key, nextValue)} />
          <button className="row-action row-delete" onClick={() => removeEntry(index)} title="Remove">
            <Trash2 size={13} />
          </button>
        </div>
      ))}
      <button className="row-action" onClick={() => onChange({ ...(value || {}), "": valueOptions[0]?.value || "" })}>
        <Plus size={14} />
        {addLabel}
      </button>
    </div>
  );
}

function SettingsView({ status, draft, setDraft, save, saving, refresh }) {
  const current = draft || {};
  const update = (patch) => setDraft({ ...current, ...patch });
  const providers = Array.isArray(current.providers) ? current.providers : [];
  const activeProvider = providers.find((provider) => provider.name === current.provider) || providers[0] || {};
  const models = Array.isArray(activeProvider.models) ? activeProvider.models : [];
  const activeModel = models.find((model) => model.name === current.model) || models[0] || {};
  const healthChecks = current.health?.checks || [];
  const healthByLabel = new Map(healthChecks.map((check) => [check.label, check]));

  function patchProvider(name, patch) {
    update({
      providers: providers.map((provider) => provider.name === name ? { ...provider, ...patch } : provider),
    });
  }

  function patchActiveModel(patch) {
    patchProvider(activeProvider.name, {
      models: models.map((model) => model.name === activeModel.name ? { ...model, ...patch } : model),
    });
  }

  function updateAgent(agent, patch) {
    update({
      agents: {
        ...(current.agents || {}),
        profiles: {
          ...(current.agents?.profiles || {}),
          [agent]: { ...((current.agents?.profiles || {})[agent] || { binary: agent }), ...patch },
        },
      },
    });
  }

  function updateAgentMode(agent, mode, patch) {
    const profile = (current.agents?.profiles || {})[agent] || { binary: agent };
    updateAgent(agent, {
      mode_overrides: {
        ...(profile.mode_overrides || {}),
        [mode]: { ...((profile.mode_overrides || {})[mode] || {}), ...patch },
      },
    });
  }

  function addSshHost() {
    update({
      ssh: {
        ...(current.ssh || {}),
        hosts: [
          ...((current.ssh?.hosts) || []),
          { name: `host-${((current.ssh?.hosts) || []).length + 1}`, host: "", port: 22, timeout_seconds: 900 },
        ],
      },
    });
  }

  function patchSshHost(index, patch) {
    update({
      ssh: {
        ...(current.ssh || {}),
        hosts: ((current.ssh?.hosts) || []).map((host, idx) => idx === index ? { ...host, ...patch } : host),
      },
    });
  }

  function removeSshHost(index) {
    update({
      ssh: {
        ...(current.ssh || {}),
        hosts: ((current.ssh?.hosts) || []).filter((_, idx) => idx !== index),
      },
    });
  }

  function addMcpServer() {
    update({
      mcp: {
        ...(current.mcp || {}),
        servers: [
          ...((current.mcp?.servers) || []),
          { name: `server-${((current.mcp?.servers) || []).length + 1}`, enabled: true, transport: "stdio", args: [], env: {}, headers: {}, timeout_seconds: 30 },
        ],
      },
    });
  }

  function patchMcpServer(index, patch) {
    update({
      mcp: {
        ...(current.mcp || {}),
        servers: ((current.mcp?.servers) || []).map((server, idx) => idx === index ? { ...server, ...patch } : server),
      },
    });
  }

  function removeMcpServer(index) {
    update({
      mcp: {
        ...(current.mcp || {}),
        servers: ((current.mcp?.servers) || []).filter((_, idx) => idx !== index),
      },
    });
  }

  const providerOptions = providers.map((provider) => ({ value: provider.name, label: provider.name }));
  const modelOptions = models.map((model) => ({ value: model.name, label: model.name }));
  const availableAgentIds = Array.isArray(current.available_agents)
    ? current.available_agents
        .map((agent) => (typeof agent === "string" ? agent : agent?.id))
        .filter(Boolean)
    : AGENT_IDS;
  const agentOptions = availableAgentIds.map((id) => ({ value: id, label: id }));
  const firstAvailableAgent = availableAgentIds[0] || "";
  const selectedDefaultAgent = availableAgentIds.includes(current.agents?.default_agent)
    ? current.agents.default_agent
    : firstAvailableAgent;

  return (
    <section className="workspace-view settings-view">
      <div className="workspace-head">
        <div>
          <span>Local config</span>
          <strong>{status?.tool_count ?? 0} tools</strong>
        </div>
        <div className="head-actions">
          <button className="ghost-button" onClick={refresh}>
            <RefreshCw size={15} />
            <span>Refresh</span>
          </button>
          <button className="ghost-button dark" onClick={save} disabled={saving || !draft}>
            {saving ? <Loader2 size={15} className="spin" /> : <Save size={15} />}
            <span>Save</span>
          </button>
        </div>
      </div>

      <SettingsSection title="Model Providers">
        <div className="field-row">
          <span>Provider</span>
          <GlassSelect
            value={current.provider || ""}
            options={providerOptions}
            onChange={(provider) => {
              const next = providers.find((item) => item.name === provider);
              update({ provider, model: next?.models?.[0]?.name || current.model });
            }}
          />
        </div>
        <div className="field-row">
          <span>Model</span>
          <GlassSelect value={current.model || ""} options={modelOptions} onChange={(model) => update({ model })} />
        </div>
        <div className="field-row">
          <span>Protocol</span>
          <GlassSelect value={activeProvider.protocol || "openai"} options={PROVIDER_PROTOCOL_OPTIONS} onChange={(protocol) => patchProvider(activeProvider.name, { protocol })} />
        </div>
        <div className="field-row">
          <span>API</span>
          <GlassSelect value={activeProvider.api || "chat_completions"} options={PROVIDER_API_OPTIONS} onChange={(api) => patchProvider(activeProvider.name, { api })} />
        </div>
        <label className="field-row"><span>Base URL</span><input value={activeProvider.base_url || ""} onChange={(event) => patchProvider(activeProvider.name, { base_url: event.target.value })} /></label>
        <label className="field-row"><span>API key env</span><input value={activeProvider.api_key_env || ""} onChange={(event) => patchProvider(activeProvider.name, { api_key_env: event.target.value || null })} placeholder={activeProvider.api_key_configured ? "configured" : "OPENAI_API_KEY"} /></label>
        <div className="field-row">
          <span>API key</span>
          <SecretInput
            value={activeProvider.api_key || ""}
            placeholder={activeProvider.api_key_configured ? "configured" : "not configured"}
            onChange={(api_key) => patchProvider(activeProvider.name, { api_key })}
          />
        </div>
        <label className="field-row"><span>Provider timeout</span><input type="number" value={activeProvider.request_timeout_seconds || 0} onChange={(event) => patchProvider(activeProvider.name, { request_timeout_seconds: parsePositiveInt(event.target.value, 600) })} /></label>
        <label className="switch-row"><ShieldCheck size={16} /><span>Stream</span><input type="checkbox" checked={Boolean(activeProvider.stream)} onChange={(event) => patchProvider(activeProvider.name, { stream: event.target.checked })} /></label>
        <label className="field-row"><span>Model capacity input</span><input type="number" value={activeModel.max_input_tokens || 0} onChange={(event) => patchActiveModel({ max_input_tokens: parsePositiveInt(event.target.value, 1024) })} /></label>
        <label className="field-row"><span>Model capacity output</span><input type="number" value={activeModel.max_output_tokens || 0} onChange={(event) => patchActiveModel({ max_output_tokens: parsePositiveInt(event.target.value, 4096) })} /></label>
        <label className="switch-row"><ShieldCheck size={16} /><span>Thinking</span><input type="checkbox" checked={Boolean(current.thinking)} onChange={(event) => update({ thinking: event.target.checked })} /></label>
        <label className="switch-row"><Bot size={16} /><span>Show thinking</span><input type="checkbox" checked={Boolean(current.show_thinking)} onChange={(event) => update({ show_thinking: event.target.checked })} /></label>
        <label className="field-row wide"><span>Provider extra body</span><JsonEditor value={activeProvider.extra_body || {}} onChange={(extra_body) => patchProvider(activeProvider.name, { extra_body })} /></label>
      </SettingsSection>

      <SettingsSection title="Context">
        <label className="field-row"><span>Context input cap</span><input type="number" value={current.context?.max_input_tokens || 0} onChange={(event) => update({ context: { ...(current.context || {}), max_input_tokens: parsePositiveInt(event.target.value, 128000) } })} /></label>
        <label className="field-row"><span>Recent turns</span><input type="number" value={current.context?.keep_recent_turns || 0} onChange={(event) => update({ context: { ...(current.context || {}), keep_recent_turns: parsePositiveInt(event.target.value, 8) } })} /></label>
      </SettingsSection>

      <SettingsSection title="Code Agents">
        {availableAgentIds.length === 0 ? (
          <div className="settings-empty">No logged-in local code agents found.</div>
        ) : (
          <>
            <div className="field-row"><span>Default agent</span><GlassSelect value={selectedDefaultAgent} options={agentOptions} onChange={(default_agent) => update({ agents: { ...(current.agents || {}), default_agent } })} /></div>
            {ROUTE_IDS.map((route) => {
              const routeAgent = current.agents?.route_defaults?.[route];
              const selectedRouteAgent = availableAgentIds.includes(routeAgent) ? routeAgent : selectedDefaultAgent;
              return (
                <div className="field-row" key={route}><span>{route} route</span><GlassSelect value={selectedRouteAgent} options={agentOptions} onChange={(agent) => update({ agents: { ...(current.agents || {}), route_defaults: { ...(current.agents?.route_defaults || {}), [route]: agent } } })} /></div>
              );
            })}
          </>
        )}
        {availableAgentIds.map((agent) => {
          const profile = current.agents?.profiles?.[agent] || { binary: agent };
          const health = healthByLabel.get(`agent cli ${agent}`);
          return (
            <div className="settings-card wide" key={agent}>
              <div className="settings-card-head"><strong>{agent}</strong><span className={`health-pill ${health?.status || "info"}`}>{health?.status || "unknown"}</span></div>
              <label><span>Binary</span><input value={profile.binary || agent} onChange={(event) => updateAgent(agent, { binary: event.target.value })} /></label>
              <div><span>Model</span><GlassSelect value={profile.model || ""} options={agentModelOptions(current, agent, profile.model)} onChange={(model) => updateAgent(agent, { model: model || null })} /></div>
              <div><span>Effort</span><GlassSelect value={normalizeAgentEffortValue(agent, profile.effort || "high")} options={agentEffortOptions(agent)} disabled={agentEffortOptions(agent).length === 0} onChange={(effort) => updateAgent(agent, { effort })} /></div>
              <label><span>Timeout</span><input type="number" value={profile.timeout_seconds || 900} onChange={(event) => updateAgent(agent, { timeout_seconds: parsePositiveInt(event.target.value, 900) })} /></label>
              <label className="inline-check"><input type="checkbox" checked={Boolean(profile.enabled ?? true)} onChange={(event) => updateAgent(agent, { enabled: event.target.checked })} /> enabled</label>
              {MODE_IDS.map((mode) => {
                const modeConfig = profile.mode_overrides?.[mode] || {};
                return (
                  <div className="mode-row" key={mode}>
                    <span>{mode}</span>
                    <GlassSelect value={normalizeAgentEffortValue(agent, modeConfig.effort || "", "")} options={agentEffortOptions(agent, true)} disabled={agentEffortOptions(agent).length === 0} onChange={(effort) => updateAgentMode(agent, mode, { effort: effort || null })} />
                    <GlassSelect value={modeConfig.model || ""} options={agentModelOptions(current, agent, modeConfig.model, "inherit")} onChange={(model) => updateAgentMode(agent, mode, { model: model || null })} />
                    <label><input type="checkbox" checked={Boolean(modeConfig.yolo)} onChange={(event) => updateAgentMode(agent, mode, { yolo: event.target.checked })} /> yolo</label>
                  </div>
                );
              })}
            </div>
          );
        })}
      </SettingsSection>

      <SettingsSection title="Workflow">
        <div className="field-row"><span>Default kind</span><GlassSelect value={current.workflow?.default_kind || "auto"} options={WORKFLOW_KIND_OPTIONS} onChange={(default_kind) => update({ workflow: { ...(current.workflow || {}), default_kind } })} /></div>
        <label className="field-row"><span>Max steps</span><input type="number" value={current.workflow?.max_steps || 64} onChange={(event) => update({ workflow: { ...(current.workflow || {}), max_steps: parsePositiveInt(event.target.value, 64) } })} /></label>
        <label className="field-row"><span>Auto route threshold</span><input type="number" step="0.1" value={current.workflow?.auto_route_threshold || 0.5} onChange={(event) => update({ workflow: { ...(current.workflow || {}), auto_route_threshold: Number(event.target.value) || 0.5 } })} /></label>
        <label className="switch-row"><GitBranch size={16} /><span>Show planned graph</span><input type="checkbox" checked={Boolean(current.workflow?.show_planned_graph ?? true)} onChange={(event) => update({ workflow: { ...(current.workflow || {}), show_planned_graph: event.target.checked } })} /></label>
        <label className="switch-row"><MessageSquare size={16} /><span>Simple chat bypass</span><input type="checkbox" checked={Boolean(current.workflow?.simple_chat_bypass_workflow ?? true)} onChange={(event) => update({ workflow: { ...(current.workflow || {}), simple_chat_bypass_workflow: event.target.checked } })} /></label>
      </SettingsSection>

      <SettingsSection title="SSH" action={<button className="row-action" onClick={addSshHost}><Plus size={14} />Host</button>}>
        {((current.ssh?.hosts) || []).map((host, index) => (
          <div className="settings-card wide" key={`${host.name}-${index}`}>
            <div className="settings-card-head"><strong>{host.name || "SSH host"}</strong><button className="row-action row-delete" onClick={() => removeSshHost(index)}><Trash2 size={13} /></button></div>
            <label><span>Alias</span><input value={host.name || ""} onChange={(event) => patchSshHost(index, { name: event.target.value })} /></label>
            <label><span>Host</span><input value={host.host || ""} onChange={(event) => patchSshHost(index, { host: event.target.value })} /></label>
            <label><span>Port</span><input type="number" value={host.port || 22} onChange={(event) => patchSshHost(index, { port: parsePositiveInt(event.target.value, 22) })} /></label>
            <label><span>Username</span><input value={host.username || ""} onChange={(event) => patchSshHost(index, { username: event.target.value || null })} /></label>
            <label><span>Private key path</span><input value={host.private_key_path || ""} onChange={(event) => patchSshHost(index, { private_key_path: event.target.value || null })} /></label>
            <label><span>Password env</span><input value={host.password_env || ""} onChange={(event) => patchSshHost(index, { password_env: event.target.value || null })} /></label>
            <label><span>Passphrase env</span><input value={host.key_passphrase_env || ""} onChange={(event) => patchSshHost(index, { key_passphrase_env: event.target.value || null })} /></label>
            <label><span>Known hosts</span><input value={host.known_hosts_path || ""} onChange={(event) => patchSshHost(index, { known_hosts_path: event.target.value || null })} /></label>
            <label><span>Default cwd</span><input value={host.default_cwd || ""} onChange={(event) => patchSshHost(index, { default_cwd: event.target.value || null })} /></label>
            <label><span>Timeout</span><input type="number" value={host.timeout_seconds || 900} onChange={(event) => patchSshHost(index, { timeout_seconds: parsePositiveInt(event.target.value, 900) })} /></label>
            <label className="inline-check"><input type="checkbox" checked={Boolean(host.accept_unknown_host_key)} onChange={(event) => patchSshHost(index, { accept_unknown_host_key: event.target.checked })} /> accept unknown host key</label>
            <label className="inline-check"><input type="checkbox" checked={Boolean(host.learn_unknown_host_key)} onChange={(event) => patchSshHost(index, { learn_unknown_host_key: event.target.checked })} /> learn unknown host key</label>
          </div>
        ))}
      </SettingsSection>

      <SettingsSection title="Tools & Approval">
        <label className="field-row wide"><span>Allowlist</span><input value={listToCsv(current.tools?.allowlist)} onChange={(event) => update({ tools: { ...(current.tools || {}), allowlist: csvToList(event.target.value) } })} /></label>
        <label className="field-row wide"><span>Denylist</span><input value={listToCsv(current.tools?.denylist)} onChange={(event) => update({ tools: { ...(current.tools || {}), denylist: csvToList(event.target.value) } })} /></label>
        <label className="field-row wide"><span>Disabled</span><input value={listToCsv(current.tools?.disabled)} onChange={(event) => update({ tools: { ...(current.tools || {}), disabled: csvToList(event.target.value) } })} /></label>
        <label className="field-row"><span>Default timeout</span><input type="number" value={current.tools?.default_timeout_seconds || ""} onChange={(event) => update({ tools: { ...(current.tools || {}), default_timeout_seconds: event.target.value ? parsePositiveInt(event.target.value, 60) : null } })} /></label>
        <div className="field-row"><span>Network policy</span><GlassSelect value={current.tools?.network_policy || "allow"} options={NETWORK_POLICY_OPTIONS} onChange={(network_policy) => update({ tools: { ...(current.tools || {}), network_policy } })} /></div>
        <div className="field-row wide"><span>Risk overrides</span><KeyValueSelectMap value={current.tools?.risk_overrides || {}} valueOptions={RISK_LEVEL_OPTIONS} addLabel="Risk" onChange={(risk_overrides) => update({ tools: { ...(current.tools || {}), risk_overrides } })} /></div>
        <div className="field-row wide"><span>Approval overrides</span><KeyValueSelectMap value={current.tools?.approval_overrides || {}} valueOptions={APPROVAL_MODE_OPTIONS} addLabel="Approval" onChange={(approval_overrides) => update({ tools: { ...(current.tools || {}), approval_overrides } })} /></div>
      </SettingsSection>

      <SettingsSection title="Integrations">
        <label className="field-row"><span>MinerU URL</span><input value={current.mineru?.base_url || ""} onChange={(event) => update({ mineru: { ...(current.mineru || {}), base_url: event.target.value } })} /></label>
        <div className="field-row">
          <span>MinerU API token</span>
          <SecretInput
            value={current.mineru?.token || ""}
            placeholder={current.mineru?.token_configured ? "configured" : "not configured"}
            onChange={(token) => update({ mineru: { ...(current.mineru || {}), token } })}
          />
        </div>
        <label className="field-row"><span>MinerU timeout</span><input type="number" value={current.mineru?.request_timeout_seconds || 600} onChange={(event) => update({ mineru: { ...(current.mineru || {}), request_timeout_seconds: parsePositiveInt(event.target.value, 600) } })} /></label>
        <label className="field-row"><span>AnySearch endpoint</span><input value={current.anysearch?.endpoint || ""} onChange={(event) => update({ anysearch: { ...(current.anysearch || {}), endpoint: event.target.value } })} /></label>
        <div className="field-row">
          <span>AnySearch API key</span>
          <SecretInput
            value={current.anysearch?.api_key || ""}
            placeholder={current.anysearch?.api_key_configured ? "configured" : "not configured"}
            onChange={(api_key) => update({ anysearch: { ...(current.anysearch || {}), api_key } })}
          />
        </div>
        <label className="field-row"><span>AnySearch timeout</span><input type="number" value={current.anysearch?.request_timeout_seconds || 60} onChange={(event) => update({ anysearch: { ...(current.anysearch || {}), request_timeout_seconds: parsePositiveInt(event.target.value, 60) } })} /></label>
        <label className="field-row"><span>AnySearch / min</span><input type="number" value={current.anysearch?.rate_limit?.max_per_minute || 60} onChange={(event) => update({ anysearch: { ...(current.anysearch || {}), rate_limit: { ...(current.anysearch?.rate_limit || {}), max_per_minute: parsePositiveInt(event.target.value, 60) } } })} /></label>
        <label className="field-row"><span>AnySearch parallel</span><input type="number" value={current.anysearch?.rate_limit?.max_parallel || 4} onChange={(event) => update({ anysearch: { ...(current.anysearch || {}), rate_limit: { ...(current.anysearch?.rate_limit || {}), max_parallel: parsePositiveInt(event.target.value, 4) } } })} /></label>
        <label className="field-row"><span>AnySearch retries</span><input type="number" value={current.anysearch?.rate_limit?.max_retries || 2} onChange={(event) => update({ anysearch: { ...(current.anysearch || {}), rate_limit: { ...(current.anysearch?.rate_limit || {}), max_retries: parsePositiveInt(event.target.value, 2) } } })} /></label>
        <label className="switch-row"><Search size={16} /><span>Retry on 429</span><input type="checkbox" checked={Boolean(current.anysearch?.rate_limit?.retry_on_429 ?? true)} onChange={(event) => update({ anysearch: { ...(current.anysearch || {}), rate_limit: { ...(current.anysearch?.rate_limit || {}), retry_on_429: event.target.checked } } })} /></label>
        <label className="field-row"><span>GitHub API URL</span><input value={current.github?.api_url || ""} onChange={(event) => update({ github: { ...(current.github || {}), api_url: event.target.value } })} /></label>
        <div className="field-row">
          <span>GitHub token</span>
          <SecretInput
            value={current.github?.token || ""}
            placeholder={current.github?.token_configured ? "configured" : "not configured"}
            onChange={(token) => update({ github: { ...(current.github || {}), token } })}
          />
        </div>
        <label className="field-row"><span>GitHub timeout</span><input type="number" value={current.github?.request_timeout_seconds || 30} onChange={(event) => update({ github: { ...(current.github || {}), request_timeout_seconds: parsePositiveInt(event.target.value, 30) } })} /></label>
      </SettingsSection>

      <SettingsSection title="Memory">
        <label className="switch-row"><MemoryStick size={16} /><span>Memory enabled</span><input type="checkbox" checked={Boolean(current.memory?.enabled ?? true)} onChange={(event) => update({ memory: { ...(current.memory || {}), enabled: event.target.checked } })} /></label>
        <label className="switch-row"><Sparkles size={16} /><span>Auto consider write</span><input type="checkbox" checked={Boolean(current.memory?.auto_consider_write ?? true)} onChange={(event) => update({ memory: { ...(current.memory || {}), auto_consider_write: event.target.checked } })} /></label>
        <div className="field-row"><span>Embedding model</span><GlassSelect value={current.memory?.embedding?.model || "bge-small-zh-v1.5"} options={MEMORY_EMBEDDING_MODEL_OPTIONS} onChange={(model) => update({ memory: { ...(current.memory || {}), embedding: { ...(current.memory?.embedding || {}), model } } })} /></div>
        <div className="field-row"><span>Download policy</span><GlassSelect value={current.memory?.embedding?.download || "ask"} options={MEMORY_DOWNLOAD_OPTIONS} onChange={(download) => update({ memory: { ...(current.memory || {}), embedding: { ...(current.memory?.embedding || {}), download } } })} /></div>
      </SettingsSection>

      <SettingsSection title="MCP & Skills" action={<button className="row-action" onClick={addMcpServer}><Plus size={14} />Server</button>}>
        {((current.mcp?.servers) || []).map((server, index) => (
          <div className="settings-card wide" key={`${server.name}-${index}`}>
            <div className="settings-card-head"><strong>{server.name || "MCP server"}</strong><button className="row-action row-delete" onClick={() => removeMcpServer(index)}><Trash2 size={13} /></button></div>
            <label><span>Name</span><input value={server.name || ""} onChange={(event) => patchMcpServer(index, { name: event.target.value })} /></label>
            <label className="inline-check"><input type="checkbox" checked={Boolean(server.enabled ?? true)} onChange={(event) => patchMcpServer(index, { enabled: event.target.checked })} /> enabled</label>
            <div><span>Transport</span><GlassSelect value={server.transport || "stdio"} options={MCP_TRANSPORT_OPTIONS} onChange={(transport) => patchMcpServer(index, { transport })} /></div>
            <label><span>Command</span><input value={server.command || ""} onChange={(event) => patchMcpServer(index, { command: event.target.value || null })} /></label>
            <label><span>Args</span><input value={listToCsv(server.args)} onChange={(event) => patchMcpServer(index, { args: csvToList(event.target.value) })} /></label>
            <label><span>URL</span><input value={server.url || ""} onChange={(event) => patchMcpServer(index, { url: event.target.value || null })} /></label>
            <label><span>Timeout</span><input type="number" value={server.timeout_seconds || 30} onChange={(event) => patchMcpServer(index, { timeout_seconds: parsePositiveInt(event.target.value, 30) })} /></label>
            <label><span>Env</span><JsonEditor value={server.env || {}} onChange={(env) => patchMcpServer(index, { env })} /></label>
            <label><span>Headers</span><JsonEditor value={server.headers || {}} onChange={(headers) => patchMcpServer(index, { headers })} /></label>
          </div>
        ))}
        <label className="field-row wide"><span>Skill roots</span><input value={listToCsv(current.skills?.roots)} disabled /></label>
      </SettingsSection>

      <SettingsSection title="Advanced">
        <label className="field-row wide"><span>Config preview</span><JsonEditor value={current} onChange={(next) => setDraft(next)} rows={12} /></label>
      </SettingsSection>

      <div className="settings-meta">
        <span>Registry {status?.registry_version ?? "-"}</span>
        <span>{status?.loaded_skills ?? 0} skills</span>
        <span>{status?.task_count ?? 0} tasks</span>
      </div>
    </section>
  );
}

function IconRail({ activeNav, setActiveNav, historyOpen, setHistoryOpen }) {
  return (
    <aside className="icon-rail">
      <button className="brand-mark" aria-label="pw home">
        pw
      </button>
      <nav>
        {NAV_ITEMS.map((item) => {
          const Icon = item.icon;
          return (
            <button
              key={item.id}
              className={`rail-button ${activeNav === item.id ? "active" : ""}`}
              title={item.label}
              onClick={() => setActiveNav(item.id)}
            >
              <Icon size={19} strokeWidth={1.8} />
            </button>
          );
        })}
      </nav>
      {activeNav === "workbench" && !historyOpen && (
        <button className="rail-button rail-chat-toggle" title="Expand chats" aria-label="Expand chats" onClick={() => setHistoryOpen(true)}>
          <ChevronRight size={17} />
        </button>
      )}
    </aside>
  );
}

function HistorySidebar({
  folders,
  folderOpen,
  setFolderOpen,
  sessionsByFolder,
  sessions,
  historyTab,
  setHistoryTab,
  search,
  setSearch,
  addFolder,
  activeSession,
  openSession,
  assignSession,
  deleteSession,
  draggingSessionId,
  setDraggingSessionId,
  onResize,
  onCollapse,
}) {
  const [folderDraft, setFolderDraft] = useState("");
  const [folderSubmitting, setFolderSubmitting] = useState(false);
  const folderInputRef = useRef(null);
  const [dragOverFolder, setDragOverFolder] = useState(null);

  async function submitFolder(event) {
    event.preventDefault();
    const name = folderDraft.trim();
    if (!name) {
      folderInputRef.current?.focus();
      return;
    }
    setFolderSubmitting(true);
    const ok = await addFolder(name);
    setFolderSubmitting(false);
    if (ok) setFolderDraft("");
  }

  function clearDragState() {
    setDraggingSessionId(null);
    setDragOverFolder(null);
  }

  return (
    <aside className="history-sidebar">
      <div className="history-inner">
        <div className="history-header">
          <h2>Chats</h2>
          <div className="history-header-actions">
            <button className="icon-button strong" onClick={() => folderInputRef.current?.focus()} title="New folder">
              <Plus size={19} />
            </button>
            <button className="history-collapse-button" onClick={onCollapse} title="Hide chats">
              <PanelLeftClose size={16} />
              <span>Hide</span>
            </button>
          </div>
        </div>
        <label className="search-box">
          <Search size={18} />
          <input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Search chats"
            aria-label="Search chats"
          />
          <kbd>⌘K</kbd>
        </label>
        <div className="history-tabs">
          <button className={historyTab === "folders" ? "active" : ""} onClick={() => setHistoryTab("folders")}>Folders</button>
          <button className={historyTab === "recent" ? "active" : ""} onClick={() => setHistoryTab("recent")}>Recent</button>
        </div>
        {historyTab === "folders" ? (
          <div className="folder-list">
            <form className="new-folder-form" onSubmit={submitFolder}>
              <FolderPlus size={17} />
              <input
                ref={folderInputRef}
                value={folderDraft}
                onChange={(event) => setFolderDraft(event.target.value)}
                placeholder="New folder"
                aria-label="New folder name"
              />
              <button disabled={folderSubmitting || !folderDraft.trim()} title="Create folder">
                {folderSubmitting ? <Loader2 size={15} className="spin" /> : <Plus size={15} />}
              </button>
            </form>
            {folders.map((folder) => {
              const folderSessions = sessionsByFolder.get(folder.id) || [];
              const isOpen = folderOpen[folder.id] ?? false;
              return (
                <div className="folder-group" key={folder.id}>
                  <button
                    className={`folder-row ${dragOverFolder === folder.id ? "drag-over" : ""}`}
                    onClick={() => setFolderOpen((prev) => ({ ...prev, [folder.id]: !isOpen }))}
                    aria-expanded={isOpen}
                    onDragOver={(event) => {
                      if (!draggingSessionId) return;
                      event.preventDefault();
                      setDragOverFolder(folder.id);
                    }}
                    onDragEnter={(event) => {
                      if (!draggingSessionId) return;
                      event.preventDefault();
                      setDragOverFolder(folder.id);
                    }}
                    onDragLeave={(event) => {
                      if (!draggingSessionId) return;
                      if (!event.currentTarget.contains(event.relatedTarget)) {
                        setDragOverFolder((current) => (current === folder.id ? null : current));
                      }
                    }}
                    onDrop={(event) => {
                      event.preventDefault();
                      const sessionId = event.dataTransfer.getData("text/plain") || draggingSessionId;
                      if (sessionId) assignSession(sessionId, folder.id);
                      clearDragState();
                    }}
                  >
                    <Folder size={17} />
                    <span>{folder.name}</span>
                    <span className="folder-count">{folderSessions.length || ""}</span>
                    <ChevronDown size={15} className={!isOpen ? "rotate" : ""} />
                  </button>
                  {isOpen && (
                    <div className="thread-list">
                      {folderSessions.length === 0 ? (
                        <div className="empty-folder">No chats yet</div>
                      ) : (
                        folderSessions.map((entry) => (
                          <ThreadRow
                            key={entry.id}
                            entry={entry}
                            active={activeSession?.id === entry.id}
                            onClick={() => openSession(entry)}
                            onDragStart={setDraggingSessionId}
                            onDragEnd={clearDragState}
                            draggingSessionId={draggingSessionId}
                            onDelete={() => deleteSession(entry.id)}
                          />
                        ))
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        ) : (
          <div className="recent-list">
            {sessions.length === 0 ? (
              <div className="history-empty">
                <Inbox size={18} />
                <span>No previous chats</span>
              </div>
            ) : (
              sessions.map((entry) => (
                <ThreadRow
                  key={entry.id}
                  entry={entry}
                  active={activeSession?.id === entry.id}
                  onClick={() => openSession(entry)}
                  onDragStart={setDraggingSessionId}
                  onDragEnd={clearDragState}
                  draggingSessionId={draggingSessionId}
                  onDelete={() => deleteSession(entry.id)}
                  showTime
                />
              ))
            )}
          </div>
        )}
      </div>
      <div className="history-resizer" onMouseDown={onResize} />
    </aside>
  );
}

function ThreadRow({
  entry,
  active,
  onClick,
  showTime,
  onDragStart,
  onDragEnd,
  draggingSessionId,
  onDelete,
}) {
  const isDragging = draggingSessionId === entry.id;

  function onDragStartHandler(event) {
    onDragStart?.(entry.id);
    event.dataTransfer.setData("text/plain", entry.id);
    event.dataTransfer.effectAllowed = "move";
  }

  function onDeleteHandler(event) {
    event.preventDefault();
    event.stopPropagation();
    onDelete?.();
  }

  return (
    <div
      className={`thread-row ${active ? "active" : ""} ${isDragging ? "dragging" : ""}`}
      draggable
      onDragStart={onDragStartHandler}
      onDragEnd={() => onDragEnd?.(entry.id)}
      onClick={onClick}
    >
      <span className="thread-title">{compactThreadTitle(entry)}</span>
      {showTime && <span className="thread-time">{formatSessionTime(entry.modified_at_ms)}</span>}
      {!showTime && active && <span className="active-dot" />}
      <GripVertical size={14} className="thread-drag-handle" />
      <button type="button" className="thread-action" title="Delete chat" aria-label={`Delete chat ${compactThreadTitle(entry)}`} onClick={onDeleteHandler}>
        <Trash2 size={14} />
      </button>
    </div>
  );
}

function WorkflowStrip({ plan, nodes, activeNode, nodeStates, runState }) {
  const resolvedKind = workflowKindLabel(plan?.resolved_kind);
  return (
    <div className="workflow-strip">
      <div className="route-chip">
        <Sparkles size={14} />
        <span>Route</span>
        {resolvedKind !== "auto" ? <strong>{resolvedKind}</strong> : null}
      </div>
      <div className="workflow-nodes">
        {nodes.map((id, index) => {
          const node = plan?.workflow?.nodes?.[id];
          const status = nodeStates?.[id];
          const error = status === "failure" || status === "interrupt" || status === "error";
          const complete = status === "success" || status === "done" || (activeNode && nodes.indexOf(activeNode) > index && !error);
          const active = !error && !complete && (status === "running" || activeNode === id || (!activeNode && index === 0 && runState !== "idle"));
          return (
            <div key={id} className={`workflow-node ${active ? "active" : ""} ${complete ? "complete" : ""} ${error ? "error" : ""}`}>
              {error ? <X size={13} /> : complete ? <Check size={13} /> : active ? <Loader2 size={13} className="spin" /> : <Circle size={10} />}
              <span>{nodeLabel(id, node)}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Message({ message, thinkingOpen, setThinkingOpen, resolveApproval }) {
  const [copied, setCopied] = useState("");
  const isUser = message.role === "user";
  const isMeta = message.role === "tool" || message.role === "system";
  const content = message.content || (message.role === "assistant" ? "Working..." : "");
  async function copyMessage(format) {
    const raw = String(message.content || content || "");
    const value = format === "text" ? markdownToPlainText(raw) : raw;
    await copyTextToClipboard(value);
    setCopied(format);
    window.setTimeout(() => setCopied(""), 1200);
  }
  if (isMeta) {
    return (
      <article className="message assistant meta-message">
        <div className="avatar"><TerminalSquare size={15} /></div>
        <div className="message-body">
          <details className={`meta-message-details ${message.is_error ? "error" : ""}`}>
            <summary>{message.role === "tool" ? `Tool output${message.tool_call_id ? ` · ${shortId(message.tool_call_id)}` : ""}` : "System message"}</summary>
            <pre>{content}</pre>
          </details>
        </div>
      </article>
    );
  }
  return (
    <article className={`message ${isUser ? "user" : "assistant"}`}>
      <div className="avatar">{isUser ? "You" : <Bot size={16} />}</div>
      <div className="message-body">
        <div className="message-copy-actions" aria-label="Copy message">
          <button type="button" onClick={() => copyMessage("md")} title="Copy as Markdown">
            MD
          </button>
          <button type="button" onClick={() => copyMessage("text")} title="Copy as plain text">
            Text
          </button>
          {copied && <small>Copied {copied === "md" ? "MD" : "text"}</small>}
        </div>
        <div className={`message-content ${isUser ? "plain" : "markdown"}`}>
          {isUser ? (
            content
          ) : (
            <ReactMarkdown
              remarkPlugins={[remarkGfm]}
              components={{
                a: ({ node: _node, ...props }) => (
                  <a {...props} target="_blank" rel="noreferrer" />
                ),
              }}
            >
              {content}
            </ReactMarkdown>
          )}
        </div>
        {message.thinking && (
          <button className="thinking-row" onClick={() => setThinkingOpen(!thinkingOpen)}>
            <ChevronDown size={15} className={!thinkingOpen ? "rotate" : ""} />
            <span>Thinking</span>
          </button>
        )}
        {thinkingOpen && message.thinking && <pre className="thinking-content">{message.thinking}</pre>}
        {message.tools?.length > 0 && (
          <div className="tool-list">
            {message.tools.map((tool) => (
              <div className="tool-row" key={tool.id}>
                <div className="tool-row-head">
                  <Wrench size={15} />
                  <span>{tool.name || "Tool call"}</span>
                  <small>{tool.status}</small>
                </div>
                {tool.summary && <p>{tool.summary}</p>}
                {(tool.details?.length > 0 || tool.progress?.length > 0) && (
                  <details className="tool-details">
                    <summary>Details</summary>
                    <ul>
                      {(tool.details || []).map((detail, index) => (
                        <li key={`detail-${index}`}>{detail}</li>
                      ))}
                      {(tool.progress || []).map((line, index) => (
                        <li key={`progress-${index}`}>{line}</li>
                      ))}
                    </ul>
                  </details>
                )}
              </div>
            ))}
          </div>
        )}
        {message.approval && (
          <div className={`approval-row ${message.approval.status ? "settled" : ""}`}>
            <TerminalSquare size={16} />
            <span>{message.approval.prompt || "Approve tool call"}</span>
            {message.approval.status ? (
              <small>{message.approval.status}</small>
            ) : (
              <>
                <button onClick={() => resolveApproval(message.approval.id, false)}>Reject</button>
                <button className="primary" onClick={() => resolveApproval(message.approval.id, true)}>Approve</button>
              </>
            )}
          </div>
        )}
      </div>
    </article>
  );
}

function StatusLine({ label }) {
  return (
    <div className="status-line">
      <Loader2 size={14} className="spin" />
      <span>{label}</span>
      <span className="status-muted">Background task running</span>
    </div>
  );
}

function MaterialsPanel({ taskId, data, loading, openMaterial, openArtifact, refresh, postprocess }) {
  const groups = data?.groups || {};
  const pdfs = Array.isArray(groups.pdf) ? groups.pdf : [];
  const searches = Array.isArray(groups.search) ? groups.search : [];
  const reports = Array.isArray(groups.reports) ? groups.reports : [];
  const extraction = groups.memory_extraction || {};
  const extractionPapers = Array.isArray(extraction.papers) ? extraction.papers : [];
  const extractionByArtifact = new Map(
    extractionPapers
      .filter((item) => item?.artifact_id)
      .map((item) => [item.artifact_id, item]),
  );
  const hasMaterials = pdfs.length > 0 || searches.length > 0 || reports.length > 0 || extractionPapers.length > 0;
  return (
    <details className="materials-panel" open={hasMaterials}>
      <summary>
        <span><Archive size={15} /> Materials</span>
        <small>{taskId ? shortId(taskId) : "workflow"}</small>
        {loading && <Loader2 size={13} className="spin" />}
      </summary>
      <div className="materials-toolbar">
        <button onClick={() => refresh?.(taskId)} disabled={!taskId || loading}>
          <RefreshCw size={13} />
          <span>Refresh</span>
        </button>
        <button onClick={() => postprocess?.(taskId)} disabled={!taskId || loading}>
          <MemoryStick size={13} />
          <span>Extract memory</span>
        </button>
      </div>
      {!data && !loading ? (
        <p className="materials-empty">No material archive loaded yet.</p>
      ) : !hasMaterials ? (
        <p className="materials-empty">No archived research materials yet.</p>
      ) : (
        <div className="materials-grid">
          {searches.length > 0 && (
            <section>
              <h3><Search size={14} /> Search</h3>
              {searches.slice(0, 4).map((item, index) => (
                <div className="material-mini" key={`search-${index}`}>
                  <strong>{clipText(item.query || item.metadata?.query || "Search", 90)}</strong>
                  <span>{item.result_count ? `${item.result_count} results` : clipText(item.preview, 120)}</span>
                </div>
              ))}
            </section>
          )}
          {pdfs.length > 0 && (
            <section>
              <h3><Database size={14} /> PDF / Markdown</h3>
              {pdfs.map((paper) => {
                const artifactId = paper.artifact_id;
                const memory = extractionByArtifact.get(artifactId);
                return (
                  <div className="material-paper" key={artifactId || paper.canonical_title}>
                    <div>
                      <strong>{paper.canonical_title || "Untitled paper"}</strong>
                      <span>
                        {paper.evidence_level || paper.read_level || "unknown"}
                        {paper.markdown_chars ? ` · ${Number(paper.markdown_chars).toLocaleString()} chars` : ""}
                        {paper.image_count ? ` · ${paper.image_count} images` : ""}
                      </span>
                      {memory && (
                        <small>
                          memory {memory.status}
                          {memory.accepted_fact_count != null ? ` · ${memory.accepted_fact_count} facts` : ""}
                          {memory.inference_count != null ? ` · ${memory.inference_count} inferences` : ""}
                          {memory.hypothesis_count != null ? ` · ${memory.hypothesis_count} hypotheses` : ""}
                        </small>
                      )}
                    </div>
                    {artifactId && (
                      <button onClick={() => openArtifact?.(artifactId)}>
                        {openMaterial?.artifact_id === artifactId ? "Hide" : "Open"}
                      </button>
                    )}
                  </div>
                );
              })}
            </section>
          )}
          {reports.length > 0 && (
            <section>
              <h3><Archive size={14} /> Reports</h3>
              {reports.map((report, index) => (
                <div className="material-mini" key={report.path || `report-${index}`}>
                  <strong>{report.title || "Workflow report"}</strong>
                  <span>{report.date || ""}{report.chars ? ` · ${Number(report.chars).toLocaleString()} chars` : ""}</span>
                  {report.path && <span>{report.path}</span>}
                </div>
              ))}
            </section>
          )}
        </div>
      )}
      {openMaterial?.loading && <StatusLine label="Loading material" />}
      {openMaterial?.error && <p className="materials-error">{openMaterial.error}</p>}
      {openMaterial?.content && (
        <details className="material-preview" open>
          <summary>{openMaterial.metadata?.canonical_title || "Markdown preview"}</summary>
          <ReactMarkdown remarkPlugins={[remarkGfm]}>
            {String(openMaterial.content || "").slice(0, 12000)}
          </ReactMarkdown>
        </details>
      )}
    </details>
  );
}

function Composer({
  value,
  setValue,
  send,
  disabled,
  focused,
  setFocused,
  workflowKind,
  workflowKinds = [],
  onWorkflowKindChange,
  status,
  showThinking,
  toggleShowThinking,
  canStop,
  stop,
}) {
  const selectedWorkflowKind = workflowKind || "auto";
  const profile = buildRunProfile(selectedWorkflowKind, status);
  return (
    <section className={`composer-wrap ${focused ? "focused" : ""}`}>
      <textarea
        value={value}
        onChange={(event) => setValue(event.target.value)}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onKeyDown={(event) => {
          if ((event.metaKey || event.ctrlKey) && event.key === "Enter") send();
        }}
        placeholder="Ask pw"
        aria-label="Ask pw"
        rows={2}
      />
      <div className="composer-toolbar">
        <div className="composer-tools">
          <GlassSelect
            value={selectedWorkflowKind}
            options={workflowKinds}
            onChange={(nextValue) => onWorkflowKindChange?.(nextValue)}
            placeholder="Mode"
          />
          <button
            className={`selector-button ${showThinking ? "active" : ""}`}
            title="Show thinking"
            onClick={toggleShowThinking}
          >
            <Bot size={15} />
            <span>Thinking</span>
          </button>
        </div>
        <button
          className={`send-button ${canStop ? "stop-inline" : ""}`}
          onClick={canStop ? stop : send}
          disabled={!canStop && (disabled || !value.trim())}
          title={canStop ? "Stop workflow" : "Send"}
        >
          {canStop ? <SquareStop size={17} /> : disabled ? <Loader2 size={18} className="spin" /> : <Send size={18} />}
        </button>
      </div>
      <div className="run-profile" aria-label="Run profile">
        {profile.map((item) => (
          <span key={item.label} title={item.title || `${item.label}: ${item.value}`}>
            <small>{item.label}</small>
            <strong>{item.value}</strong>
          </span>
        ))}
      </div>
    </section>
  );
}

function buildRunProfile(workflowKind, status) {
  const serverProfile = status?.run_profile || {};
  const provider = status?.provider || "provider";
  const model = status?.model || "model";
  const maxInput = status?.model_max_input_tokens ? `${status.model_max_input_tokens}` : "auto";
  const thinking = status?.thinking ? "on" : "off";
  if (workflowKind === "chat") {
    return [
      { label: "route", value: "chat" },
      { label: "model", value: `${provider} / ${model}` },
      { label: "context", value: maxInput },
      { label: "thinking", value: thinking },
    ];
  }
  if (AGENT_WORKFLOW_KINDS.has(workflowKind)) {
    return [
      { label: "route", value: workflowKind },
      {
        label: "agent",
        value: serverProfile.agent || "codex",
        title: "Resolved from Settings agents route defaults unless the request overrides it.",
      },
      { label: "mode", value: "workflow" },
      { label: "effort", value: serverProfile.agent_effort || "high" },
      {
        label: "model",
        value: serverProfile.agent_model || "agent default",
        title: "Configured in Settings code agent profile; empty means the local agent chooses its default.",
      },
      { label: "timeout", value: `${serverProfile.agent_timeout_seconds || 900}s` },
      { label: "context", value: "workflow pack" },
    ];
  }
  return [
    { label: "route", value: workflowKind || "auto" },
    { label: "planner", value: `${provider} / ${model}` },
    { label: "context", value: maxInput },
    { label: "thinking", value: thinking },
  ];
}

export { App };
