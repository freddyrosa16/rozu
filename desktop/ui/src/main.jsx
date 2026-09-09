import React, { useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import {
  Plus, Search, Zap, Grid2X2, Folder, Settings, PanelLeft,
  PanelRight, ChevronDown, ArrowUp, Monitor, GitBranch,
  X, FileCode2, Terminal, Puzzle, Keyboard, Check, SlidersHorizontal,
  MessageSquare, List, GitPullRequest, Sun, Moon,
} from 'lucide-react';
import { startWaves } from './waves';
import './styles.css';

const PREVIEW = 'Interface preview — no tasks will run.';
const pages = [
  { id: 'task', label: 'New task', icon: Plus },
  { id: 'search', label: 'Search', icon: Search, shortcut: '⌘ K' },
  { id: 'threads', label: 'Threads', icon: List },
  { id: 'pullrequests', label: 'Pull requests', icon: GitPullRequest },
  { id: 'automations', label: 'Automations', icon: Zap },
  { id: 'skills', label: 'Skills', icon: Grid2X2 },
];

function IconButton({ label, children, ...props }) {
  return <button className="icon-button" type="button" aria-label={label} title={label} {...props}>{children}</button>;
}

function Mark({ className = '' }) {
  return <svg className={`rozu-mark ${className}`} viewBox="0 0 40 40" aria-hidden="true"><path d="M11 34V15C11 6 16 3 25 3h5v8h-5c-4 0-6 2-6 6v17z" fill="currentColor" /></svg>;
}

function DitherBackground() {
  const canvas = useRef(null);
  useEffect(() => startWaves(canvas.current), []);
  return <canvas ref={canvas} className="dither-background" aria-hidden="true" />;
}

function App() {
  const [page, setPage] = useState('task');
  const [sidebar, setSidebar] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(() => Math.min(306, Math.max(232, window.innerWidth * .2)));
  const [sidebarLimit, setSidebarLimit] = useState(() => Math.max(180, Math.min(420, window.innerWidth - 480)));
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const [panel, setPanel] = useState(false);
  const [panelTab, setPanelTab] = useState('Files');
  const [modal, setModal] = useState(null);
  const [draft, setDraft] = useState('');
  const [query, setQuery] = useState('');
  const [theme, setTheme] = useState('dark');
  const [settingTab, setSettingTab] = useState('General');
  const promptRef = useRef(null);
  const openerRef = useRef(null);
  const dialogRef = useRef(null);
  const sidebarDrag = useRef(null);

  const resizeSidebar = (width) => setSidebarWidth(Math.max(180, Math.min(sidebarLimit, width)));
  const finishSidebarResize = () => { sidebarDrag.current = null; setResizingSidebar(false); };
  useEffect(() => {
    function fitSidebar() {
      const limit = Math.max(180, Math.min(420, window.innerWidth - 480));
      setSidebarLimit(limit);
      setSidebarWidth(width => Math.min(width, limit));
    }
    window.addEventListener('resize', fitSidebar);
    return () => window.removeEventListener('resize', fitSidebar);
  }, []);

  // All state is deliberately in memory. There is no API, storage, or native bridge.
  const openModal = (name) => { openerRef.current = document.activeElement; setModal(name); };
  const closeModal = () => { setModal(null); requestAnimationFrame(() => openerRef.current?.focus()); };
  const navigate = (next) => {
    if (next === 'search') { openModal('search'); return; }
    setPage(next);
  };

  useEffect(() => {
    function shortcuts(event) {
      const command = event.metaKey || event.ctrlKey;
      if (command && event.key.toLowerCase() === 'k') { event.preventDefault(); openModal('search'); }
      if (command && event.key.toLowerCase() === 'n') { event.preventDefault(); setPage('task'); promptRef.current?.focus(); }
      if (command && event.key === ',') { event.preventDefault(); setPage('settings'); }
      if (event.key === 'Escape') closeModal();
    }
    window.addEventListener('keydown', shortcuts);
    return () => window.removeEventListener('keydown', shortcuts);
  }, []);

  useEffect(() => {
    if (!modal) return;
    const dialog = dialogRef.current;
    dialog.querySelector('input, button')?.focus();
    function trap(event) {
      if (event.key !== 'Tab') return;
      const items = [...dialog.querySelectorAll('button:not(:disabled),input,select,[tabindex="0"]')];
      const first = items[0], last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    }
    dialog.addEventListener('keydown', trap);
    return () => dialog.removeEventListener('keydown', trap);
  }, [modal]);

  const pageTitle = page === 'task' ? 'New task' : page === 'pullrequests' ? 'Pull requests' : page.charAt(0).toUpperCase() + page.slice(1);

  return <div className={`app ${theme} ${sidebar ? '' : 'sidebar-hidden'} ${resizingSidebar ? 'resizing-sidebar' : ''}`} style={{ '--sidebar-width': `${sidebarWidth}px` }}>
    <div className="sidebar-shell" inert={modal ? true : undefined}>
    <aside id="sidebar-content" className="sidebar" aria-label="Sidebar">
      <div className="sidebar-brand"><span className="wordmark">rozu</span><IconButton label="Hide sidebar" onClick={() => setSidebar(false)}><PanelLeft /></IconButton></div>
      <nav aria-label="Main navigation">
        {pages.map(({ id, label, icon: Icon, shortcut }) => <button key={id} className={`nav-row ${page === id ? 'selected' : ''}`} onClick={() => navigate(id)} aria-current={page === id ? 'page' : undefined}><Icon /><span>{label}</span>{shortcut && <kbd>{shortcut}</kbd>}</button>)}
      </nav>
      <div className="project-heading"><span>Projects</span><IconButton label="Add project — preview" onClick={() => openModal('projects')}><Plus /></IconButton></div>
      <button className={`nav-row project ${page === 'task' ? 'selected' : ''}`} onClick={() => setPage('task')}><Folder /><span>rozu</span></button>
      <p className="no-tasks">No tasks yet</p>
      <div className="sidebar-bottom"><button className={`nav-row ${page === 'settings' ? 'selected' : ''}`} onClick={() => setPage('settings')}><Settings /><span>Settings</span></button><span className="version">v0.1.1 · UI preview</span></div>
    </aside>
    <div className="sidebar-resizer" role="separator" tabIndex={0}
      aria-label="Resize sidebar" aria-orientation="vertical" aria-controls="sidebar-content"
      aria-valuemin={180} aria-valuemax={sidebarLimit} aria-valuenow={Math.round(sidebarWidth)}
      title="Drag to resize sidebar. Double-click to reset."
      onPointerDown={event => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.focus();
        event.currentTarget.setPointerCapture(event.pointerId);
        sidebarDrag.current = { pointer: event.pointerId, x: event.clientX, width: sidebarWidth };
        setResizingSidebar(true);
      }}
      onPointerMove={event => {
        const drag = sidebarDrag.current;
        if (drag?.pointer === event.pointerId) resizeSidebar(drag.width + event.clientX - drag.x);
      }}
      onPointerUp={event => {
        if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
        finishSidebarResize();
      }}
      onPointerCancel={finishSidebarResize} onLostPointerCapture={finishSidebarResize}
      onDoubleClick={() => resizeSidebar(Math.min(306, Math.max(232, window.innerWidth * .2)))}
      onKeyDown={event => {
        const widths = { ArrowLeft: sidebarWidth - 10, ArrowRight: sidebarWidth + 10, Home: 180, End: sidebarLimit };
        if (event.key in widths) { event.preventDefault(); resizeSidebar(widths[event.key]); }
      }} />
    </div>

    <section className="workspace" inert={modal ? true : undefined}>
      {page === 'task' && <DitherBackground />}
      <header className="toolbar"><div>{!sidebar && <IconButton label="Show sidebar" onClick={() => setSidebar(true)}><PanelLeft /></IconButton>}<span className="task-tab"><MessageSquare />{pageTitle}</span><IconButton label="New task tab" onClick={() => { setPage('task'); promptRef.current?.focus(); }}><Plus /></IconButton></div><IconButton label={panel ? 'Hide workspace panel' : 'Show workspace panel'} aria-pressed={panel} onClick={() => setPanel(!panel)}><PanelRight /></IconButton></header>
      <div className="workspace-body">
        <main className={`main ${page === 'task' ? 'task-main' : ''}`}>
          {page === 'task' && <div className="new-task">
            <Mark />
            <h1>What should we build?</h1>
            <div className="composer">
              <textarea ref={promptRef} aria-label="Task prompt" placeholder="Ask Rozu to build something…" value={draft} onChange={event => setDraft(event.target.value)} spellCheck={false} />
              <div className="composer-tools">
                <div className="composer-options">
                  <IconButton label="Attachments — preview" onClick={() => openModal('attachments')}><Plus /></IconButton>
                  <span className="separator" />
                  <button className="model-picker" onClick={() => openModal('models')}>No model connected<ChevronDown /></button>
                  <button className="context-picker" aria-label="Project: rozu" onClick={() => openModal('projects')}><Folder /><span>rozu</span><ChevronDown /></button>
                  <button className="context-picker" onClick={() => openModal('environment')}><Monitor />Local<ChevronDown /></button>
                  <span className="branch-context" aria-label="Branch: main"><GitBranch />main</span>
                </div>
                <button className="send" disabled aria-label="Send unavailable in interface preview" title="No tasks will run in this interface preview"><ArrowUp /></button>
              </div>
            </div>
            <p className="preview-note">{PREVIEW}</p>
          </div>}

          {page === 'automations' && <PageContent title="Automations" description="A space for the work you want to repeat." icon={Zap} empty="No automations yet" detail="Schedules and triggers will appear here when you build the backend." button="New automation" onClick={() => openModal('automation')} />}
          {page === 'threads' && <PageContent title="Threads" description="All your conversations, in one place." icon={MessageSquare} empty="No threads yet" detail="Start with a prompt. Conversations will appear here once the backend is connected." button="New task" onClick={() => setPage('task')} />}
          {page === 'pullrequests' && <PageContent title="Pull requests" description="A home for code reviews and changes." icon={GitPullRequest} empty="No pull requests" detail="Repository connections and code reviews are not available in this preview." />}
          {page === 'skills' && <PageContent title="Skills" description="Instructions that shape how Rozu works." icon={Puzzle} empty="No skills added" detail="This is the home for your future skills library." button="Add skill" onClick={() => openModal('skill')} />}

          {page === 'settings' && <div className="settings-page"><h1>Settings</h1><div className="settings-layout"><nav className="settings-nav" aria-label="Settings sections">{['General', 'Models', 'Shortcuts', 'About'].map(item => <button key={item} className={settingTab === item ? 'selected' : ''} onClick={() => setSettingTab(item)}>{item}</button>)}</nav><div className="settings-content">
            {settingTab === 'General' && <><h2>General</h2><div className="setting-row"><div><h3>Appearance</h3><p>Choose how Rozu looks on this screen.</p></div><div className="segmented" aria-label="Appearance"><button aria-pressed={theme === 'dark'} onClick={() => setTheme('dark')}><Moon />Dark</button><button aria-pressed={theme === 'light'} onClick={() => setTheme('light')}><Sun />Light</button></div></div><div className="setting-row"><div><h3>Workspace</h3><p>No folder is connected.</p></div><span className="subtle-label">Preview</span></div><p className="settings-footnote">Appearance changes last until you close the app.</p></>}
            {settingTab === 'Models' && <><h2>Models</h2><p className="section-description">No model providers are connected.</p><div className="empty-inline"><SlidersHorizontal /><div><h3>Your models will live here</h3><p>Provider connections and credentials are not part of this preview.</p></div></div><button className="secondary-button" disabled>Connect a provider</button></>}
            {settingTab === 'Shortcuts' && <><h2>Keyboard shortcuts</h2>{[['Search', '⌘ K'], ['New task', '⌘ N'], ['Settings', '⌘ ,'], ['Close dialog', 'Esc']].map(([label, key]) => <div className="setting-row" key={label}><span>{label}</span><kbd>{key}</kbd></div>)}</>}
            {settingTab === 'About' && <><Mark className="about-mark" /><h2>rozu</h2><p className="section-description">A little curiosity. A lot of possibility.</p><div className="setting-row"><span>Version</span><span>0.1.1</span></div><div className="setting-row"><span>Build</span><span>Interface preview</span></div><div className="setting-row"><span>License</span><span>MIT</span></div><p className="settings-footnote">Frontend only. No AI calls, command execution, connected projects, or background jobs.</p></>}
          </div></div></div>}
        </main>

        {panel && <aside className="inspector" aria-label="Workspace panel"><div className="inspector-tabs">{['Files', 'Changes', 'Terminal'].map(tab => <button key={tab} className={panelTab === tab ? 'active' : ''} onClick={() => setPanelTab(tab)}>{tab}</button>)}</div><div className="inspector-empty">{panelTab === 'Terminal' ? <Terminal /> : panelTab === 'Files' ? <Folder /> : <FileCode2 />}<h3>{panelTab === 'Terminal' ? 'No terminal session' : panelTab === 'Files' ? 'No files opened' : 'No changes to review'}</h3><p>{panelTab === 'Terminal' ? 'Command execution is not connected.' : panelTab === 'Files' ? 'Your project files will appear here.' : 'Future code changes will appear here.'}</p><span className="subtle-label">Interface preview</span></div></aside>}
      </div>
    </section>

    {modal && <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) closeModal(); }}><section ref={dialogRef} role="dialog" aria-modal="true" aria-labelledby="dialog-title" className={`modal ${modal === 'search' ? 'search-modal' : ''}`}>
      {modal === 'search' ? <><div className="search-input"><Search /><input aria-label="Search tasks" placeholder="Search tasks…" value={query} onChange={event => setQuery(event.target.value)} /><button className="escape" onClick={closeModal}>Esc</button></div><h2 id="dialog-title" className="visually-hidden">Search tasks</h2><div className="search-empty"><MessageSquare /><h3>{query ? 'No matching tasks' : 'No tasks to search yet'}</h3><p>Tasks will appear here once Rozu is connected.</p></div></> : <><div className="modal-heading"><h2 id="dialog-title">{({projects:'Projects',models:'Choose a model',attachments:'Add context',environment:'Environment',automation:'New automation',skill:'Add skill'})[modal]}</h2><IconButton label="Close dialog" onClick={closeModal}><X /></IconButton></div>
        {modal === 'projects' && <><p>Choose a project for this screen.</p><button className="choice-row" onClick={() => { setPage('task'); closeModal(); }}><Folder /><span>rozu<small>Preview project · no folder connected</small></span><Check /></button><button className="secondary-button" disabled><Plus />Add project</button></>}
        {modal === 'models' && <><div className="dialog-empty"><SlidersHorizontal /><h3>No model connected</h3><p>Model selection will be available after you connect a provider.</p></div><button className="secondary-button" onClick={() => { setPage('settings'); setSettingTab('Models'); closeModal(); }}>View model settings</button></>}
        {modal === 'environment' && <><button className="choice-row" onClick={closeModal}><Monitor /><span>Local<small>Interface only · no local execution</small></span><Check /></button><p>Environment connections are not available in this preview.</p></>}
        {modal === 'attachments' && <><div className="dialog-empty"><FileCode2 /><h3>A little more context</h3><p>File attachments will live here. This preview cannot read your files.</p></div><button className="secondary-button" disabled>Attach files</button></>}
        {modal === 'automation' && <><label className="field">Name<input placeholder="e.g. Review recent changes" /></label><label className="field">Instructions<textarea placeholder="What should Rozu do?" /></label><div className="form-bottom"><span>Scheduling is not connected.</span><button className="secondary-button" disabled>Create automation</button></div></>}
        {modal === 'skill' && <><label className="field">Name<input placeholder="e.g. Code review" /></label><label className="field">Instructions<textarea placeholder="Describe how this skill should work…" /></label><div className="form-bottom"><span>Skills are not saved in this preview.</span><button className="secondary-button" disabled>Add skill</button></div></>}
      </>}
    </section></div>}
  </div>;
}

function PageContent({ title, description, icon: Icon, empty, detail, button, onClick }) {
  return <div className="collection-page"><div className="collection-header"><div><h1>{title}</h1><p>{description}</p></div>{button && <button className="secondary-button" onClick={onClick}><Plus />{button}</button>}</div><div className="collection-empty"><Icon /><h2>{empty}</h2><p>{detail}</p><span className="subtle-label">Interface preview</span></div></div>;
}

createRoot(document.getElementById('root')).render(<App />);
