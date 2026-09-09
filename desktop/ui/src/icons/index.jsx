import React, { useEffect, useRef } from 'react';
import { useReducedMotion } from 'motion/react';
import * as Static from 'lucide-react';
import { PlusIcon as PlusSource } from "./plus";
import { SearchIcon as SearchSource } from "./search";
import { ZapIcon as ZapSource } from "./zap";
import { LayoutGridIcon as Grid2X2Source } from "./layout-grid";
import { FolderOpenIcon as FolderSource } from "./folder-open";
import { SettingsIcon as SettingsSource } from "./settings";
import { PanelLeftCloseIcon as PanelLeftSource } from "./panel-left-close";
import { PanelRightOpenIcon as PanelRightSource } from "./panel-right-open";
import { ChevronDownIcon as ChevronDownSource } from "./chevron-down";
import { ArrowUpIcon as ArrowUpSource } from "./arrow-up";
import { GitBranchIcon as GitBranchSource } from "./git-branch";
import { XIcon as XSource } from "./x";
import { TerminalIcon as TerminalSource } from "./terminal";
import { CheckIcon as CheckSource } from "./check";
import { SlidersHorizontalIcon as SlidersHorizontalSource } from "./sliders-horizontal";
import { MessageSquareIcon as MessageSquareSource } from "./message-square";
import { GitPullRequestIcon as GitPullRequestSource } from "./git-pull-request";
import { SunIcon as SunSource } from "./sun";
import { MoonIcon as MoonSource } from "./moon";

// Drive the upstream icon animations from their complete button hit area.
// Reduced-motion users receive the static Lucide equivalent instead.
function animated(Source, Fallback) {
  return function AnimatedIcon(props) {
    const element = useRef(null);
    const controls = useRef(null);
    const reduced = useReducedMotion();
    useEffect(() => {
      if (reduced) return;
      const target = element.current?.closest('button') || element.current;
      if (!target || target.disabled) return;
      let hovered = false;
      let focused = false;
      const update = () => hovered || focused ? controls.current?.startAnimation() : controls.current?.stopAnimation();
      const enter = () => { hovered = true; update(); };
      const leave = () => { hovered = false; update(); };
      const focus = () => { focused = true; update(); };
      const blur = () => { focused = false; update(); };
      target.addEventListener('mouseenter', enter);
      target.addEventListener('mouseleave', leave);
      target.addEventListener('focus', focus);
      target.addEventListener('blur', blur);
      return () => {
        target.removeEventListener('mouseenter', enter);
        target.removeEventListener('mouseleave', leave);
        target.removeEventListener('focus', focus);
        target.removeEventListener('blur', blur);
      };
    }, [reduced]);
    return <span ref={element} className="animated-icon" aria-hidden="true">
      {reduced ? <Fallback {...props} /> : <Source ref={controls} {...props} />}
    </span>;
  };
}
export const Plus = animated(PlusSource, Static.Plus);
export const Search = animated(SearchSource, Static.Search);
export const Zap = animated(ZapSource, Static.Zap);
export const Grid2X2 = animated(Grid2X2Source, Static.Grid2X2);
export const Folder = animated(FolderSource, Static.Folder);
export const Settings = animated(SettingsSource, Static.Settings);
export const PanelLeft = animated(PanelLeftSource, Static.PanelLeft);
export const PanelRight = animated(PanelRightSource, Static.PanelRight);
export const ChevronDown = animated(ChevronDownSource, Static.ChevronDown);
export const ArrowUp = animated(ArrowUpSource, Static.ArrowUp);
export const GitBranch = animated(GitBranchSource, Static.GitBranch);
export const X = animated(XSource, Static.X);
export const Terminal = animated(TerminalSource, Static.Terminal);
export const Check = animated(CheckSource, Static.Check);
export const SlidersHorizontal = animated(SlidersHorizontalSource, Static.SlidersHorizontal);
export const MessageSquare = animated(MessageSquareSource, Static.MessageSquare);
export const GitPullRequest = animated(GitPullRequestSource, Static.GitPullRequest);
export const Sun = animated(SunSource, Static.Sun);
export const Moon = animated(MoonSource, Static.Moon);
export { Monitor, FileCode2, Puzzle, List } from 'lucide-react';
