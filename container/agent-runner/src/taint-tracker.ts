export type TaintSource = 'user_prompt' | 'ipc_message' | 'web_tool';

export interface TaintMatch {
  fragment: string;
  source: TaintSource;
}

export interface TaintDecision {
  reason: string;
  matches: TaintMatch[];
}

interface TaintedFragment {
  normalized: string;
  fragment: string;
  source: TaintSource;
}

const MAX_TAINTED_FRAGMENTS = 128;
const MIN_FRAGMENT_LENGTH = 8;
const TAINT_SIGNAL_TOKEN_PATTERN =
  /\b(?:bash|sh|zsh|fish|python|python3|node|perl|ruby|php|exec|eval|source|sudo|su|rm|mv|chmod|chown|dd|mkfs|mount|curl|wget)\b/i;
const HIGH_RISK_SHELL_PATTERN =
  /(?:\|\s*(?:sh|bash|zsh)\b|&&|;|\$\(|`|>\s*\/|>>)/i;
const HIGH_RISK_COMMANDS = new Set([
  'bash',
  'sh',
  'zsh',
  'fish',
  'python',
  'python3',
  'node',
  'perl',
  'ruby',
  'php',
  'sudo',
  'su',
  'rm',
  'mv',
  'chmod',
  'chown',
  'dd',
  'mkfs',
  'mount',
  'curl',
  'wget',
]);

function commandLooksHighRisk(command: string): boolean {
  if (HIGH_RISK_SHELL_PATTERN.test(command)) {
    return true;
  }

  const tokens = command.trim().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return false;

  return HIGH_RISK_COMMANDS.has(tokens[0].toLowerCase());
}

function normalizeText(text: string): string {
  return text.replace(/\s+/g, ' ').trim().toLowerCase();
}

function stripMarkup(text: string): string {
  return text
    .replace(/<[^>]+>/g, ' ')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&amp;/g, '&')
    .replace(/&quot;/g, '"');
}

function addFragment(target: Set<string>, value: string): void {
  const trimmed = value.trim();
  if (trimmed.length < MIN_FRAGMENT_LENGTH) return;
  if (trimmed.length > 160) return;
  target.add(trimmed);
}

function extractTaintFragments(text: string): string[] {
  const plain = stripMarkup(text);
  const fragments = new Set<string>();

  for (const match of plain.matchAll(/https?:\/\/[^\s"'<>]+/gi)) {
    addFragment(fragments, match[0]);
  }

  for (const match of plain.matchAll(/(?:~|\/|\.\.?\/)[A-Za-z0-9._/-]+/g)) {
    addFragment(fragments, match[0]);
  }

  for (const match of plain.matchAll(/["'`]([^"'`\n]{8,120})["'`]/g)) {
    addFragment(fragments, match[1]);
  }

  for (const line of plain.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (trimmed.length < MIN_FRAGMENT_LENGTH || trimmed.length > 160) {
      continue;
    }
    if (TAINT_SIGNAL_TOKEN_PATTERN.test(trimmed) || HIGH_RISK_SHELL_PATTERN.test(trimmed)) {
      addFragment(fragments, trimmed);
    }
  }

  return [...fragments];
}

function collectText(value: unknown, parts: string[], budget: { remaining: number }): void {
  if (budget.remaining <= 0 || value == null) return;

  if (typeof value === 'string') {
    const next = value.slice(0, budget.remaining);
    if (next) {
      parts.push(next);
      budget.remaining -= next.length;
    }
    return;
  }

  if (Array.isArray(value)) {
    for (const item of value) {
      collectText(item, parts, budget);
      if (budget.remaining <= 0) return;
    }
    return;
  }

  if (typeof value === 'object') {
    for (const entry of Object.values(value as Record<string, unknown>)) {
      collectText(entry, parts, budget);
      if (budget.remaining <= 0) return;
    }
  }
}

export class ContextualTaintTracker {
  private fragments: TaintedFragment[] = [];

  taintText(text: string, source: TaintSource): number {
    let added = 0;

    for (const fragment of extractTaintFragments(text)) {
      const normalized = normalizeText(fragment);
      if (
        this.fragments.some(
          (existing) =>
            existing.normalized === normalized && existing.source === source,
        )
      ) {
        continue;
      }

      this.fragments.push({ normalized, fragment, source });
      added++;
    }

    if (this.fragments.length > MAX_TAINTED_FRAGMENTS) {
      this.fragments.splice(0, this.fragments.length - MAX_TAINTED_FRAGMENTS);
    }

    return added;
  }

  taintToolOutput(toolName: string, output: unknown): number {
    if (toolName !== 'WebFetch' && toolName !== 'WebSearch') {
      return 0;
    }

    const parts: string[] = [];
    collectText(output, parts, { remaining: 4000 });
    return this.taintText(parts.join('\n'), 'web_tool');
  }

  inspectBashCommand(command: string): TaintDecision | null {
    const normalizedCommand = normalizeText(command);
    const matches = this.fragments.filter((fragment) =>
      normalizedCommand.includes(fragment.normalized),
    );

    if (matches.length === 0) return null;
    if (!commandLooksHighRisk(command)) {
      return null;
    }

    return {
      reason:
        'Blocked high-risk Bash command because it still contains untrusted data fragments.',
      matches: matches.slice(0, 3).map((match) => ({
        fragment: match.fragment,
        source: match.source,
      })),
    };
  }
}
