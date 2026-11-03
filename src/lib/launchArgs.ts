/**
 * 快速启动项的启动参数拆分：设置里存的是原始串（空格分隔，双/单引号段视为整体），
 * 便签点击启动时拆成数组交给 Rust。参数逐个作为独立进程参数、不拼接 shell 字符串，无注入风险。
 */

export function splitLaunchArgs(raw: string | undefined): string[] {
  if (!raw) return []
  const out: string[] = []
  // 引号段优先匹配（只作定界符），其余按空白切分
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g
  let m: RegExpExecArray | null
  while ((m = re.exec(raw))) out.push(m[1] ?? m[2] ?? m[3])
  return out
}
