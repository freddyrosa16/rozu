import React, { useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import {
  Plus, Search, Zap, Grid2X2, Folder, Settings, PanelLeft,
  PanelRight, ChevronDown, ArrowUp, Monitor, GitBranch,
  X, FileCode2, Terminal, Puzzle, Check, SlidersHorizontal,
  MessageSquare, List, GitPullRequest, Sun, Moon,
} from './icons';
import { startWaves } from './waves';
import './styles.css';

const SIDEBAR_MIN = 240;
const SIDEBAR_SNAP = 48;
const sidebarDefault = () => Math.min(306, Math.max(SIDEBAR_MIN, window.innerWidth * .2));
const sidebarMaximum = () => Math.max(SIDEBAR_MIN, Math.min(520, window.innerWidth - 440));

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
  return <svg className={`rozu-mark ${className}`} viewBox="1097 221 365 365" fill="currentColor" stroke="none" aria-hidden="true"><path d="M1230 246C1301 228 1374 272 1380 327C1384 364 1369 404 1346 425C1337 433 1328 427 1329 417C1331 393 1318 366 1294 353C1270 340 1239 339 1203 337C1184 337 1189 319 1198 301C1206 280 1214 260 1230 246Z" /><path d="M1215 359C1245 348 1280 349 1300 367C1310 377 1295 385 1279 396C1240 426 1246 475 1267 510C1273 523 1277 536 1260 538C1191 543 1120 512 1117 459C1114 415 1152 374 1215 359Z" /><path d="M1390 380C1425 404 1446 446 1435 485C1423 534 1381 562 1337 548C1295 534 1266 493 1260 464C1255 447 1261 438 1270 441C1308 461 1329 453 1350 428C1367 410 1376 376 1390 380Z" /></svg>;
}

function DitherBackground() {
  const canvas = useRef(null);
  useEffect(() => startWaves(canvas.current), []);
  return <canvas ref={canvas} className="dither-background" aria-hidden="true" />;
}

function App() {
  const [page, setPage] = useState('task');
  const [sidebar, setSidebar] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(sidebarDefault);
  const [sidebarLimit, setSidebarLimit] = useState(sidebarMaximum);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const sidebarDrag = useRef(null);
  const resizeSidebar = width => setSidebarWidth(Math.max(SIDEBAR_MIN, Math.min(sidebarLimit, width)));
  const moveSidebar = event => {
    const drag = sidebarDrag.current;
    if (drag?.pointer !== event.pointerId) return;
    const distance = event.clientX - drag.x;
    const raw = drag.width + distance;
    // Hold a readable width before snapping shut. Separate thresholds prevent
    // flicker around the snap point, including when reversing the same gesture.
    const closeAt = drag.startedOpen ? SIDEBAR_MIN - SIDEBAR_SNAP : SIDEBAR_SNAP / 2;
    const openAt = drag.startedOpen ? SIDEBAR_MIN - SIDEBAR_SNAP / 2 : SIDEBAR_SNAP;
    if (drag.open && raw < closeAt) drag.open = false;
    else if (!drag.open && raw >= openAt) drag.open = true;
    setSidebar(drag.open);
    if (drag.open) resizeSidebar(drag.startedOpen ? raw : SIDEBAR_MIN + raw - SIDEBAR_SNAP);
  };
  const finishSidebarResize = () => { sidebarDrag.current = null; setResizingSidebar(false); };
  useEffect(() => {
    const fit = () => {
      const limit = sidebarMaximum();
      setSidebarLimit(limit);
      setSidebarWidth(width => Math.max(SIDEBAR_MIN, Math.min(width, limit)));
    };
    window.addEventListener('resize', fit);
    return () => window.removeEventListener('resize', fit);
  }, []);
  const [panel, setPanel] = useState(false);
  const [panelTab, setPanelTab] = useState('Files');
  const [modal, setModal] = useState(null);
  const [draft, setDraft] = useState('');
  const [query, setQuery] = useState('');
  const [theme, setTheme] = useState('dark');
  const [settingTab, setSettingTab] = useState('General');
  const [projects, setProjects] = useState([{ id: 'rozu', name: 'rozu' }]);
  const [projectId, setProjectId] = useState('rozu');
  const [projectName, setProjectName] = useState('');
  const [files, setFiles] = useState([]);
  const [fileName, setFileName] = useState('');
  const [notice, setNotice] = useState('');
  const nextId = useRef(0);
  const project = projects.find(item => item.id === projectId);
  const removeProject = (id) => {
    const remaining = projects.filter(item => item.id !== id);
    setProjects(remaining);
    if (id === projectId) setProjectId(remaining[0]?.id ?? null);
    setNotice('Project removed from the preview.');
  };
  const removeFile = (id) => {
    setFiles(items => items.filter(item => item.id !== id));
    setNotice('File entry removed from the preview.');
  };
  const addProject = (event) => {
    event.preventDefault();
    if (!projectName.trim()) return;
    const item = { id: `project-${++nextId.current}`, name: projectName.trim() };
    setProjects(items => [...items, item]);
    setProjectId(item.id);
    setProjectName('');
    setNotice('Project added to the preview.');
  };
  const addFile = (event) => {
    event.preventDefault();
    if (!fileName.trim()) return;
    setFiles(items => [...items, { id: `file-${++nextId.current}`, name: fileName.trim() }]);
    setFileName('');
    setNotice('File entry added to the preview.');
  };
  const promptRef = useRef(null);
  const openerRef = useRef(null);
  const dialogRef = useRef(null);

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
      <div className="sidebar-brand"><span className="brand-symbol" role="img" aria-label="Rozu"><Mark /></span><IconButton label="Hide sidebar" onClick={() => setSidebar(false)}><PanelLeft /></IconButton></div>
      <nav aria-label="Main navigation">
        {pages.map(({ id, label, icon: Icon, shortcut }) => <button key={id} className={`nav-row ${page === id ? 'selected' : ''}`} onClick={() => navigate(id)} aria-current={page === id ? 'page' : undefined}><Icon /><span>{label}</span>{shortcut && <kbd>{shortcut}</kbd>}</button>)}
      </nav>
      <div className="project-heading"><span>Projects</span><IconButton label="Add project — preview" onClick={() => openModal('projects')}><Plus /></IconButton></div>
      {projects.map(item => <div className={`project-row ${item.id === projectId ? 'selected' : ''}`} key={item.id}>
        <button className="nav-row project" onClick={() => { setProjectId(item.id); setPage('task'); }}><Folder /><span>{item.name}</span></button>
        <IconButton label={`Remove project ${item.name}`} onClick={() => removeProject(item.id)}><X /></IconButton>
      </div>)}
      <p className="no-tasks">{projects.length ? 'No tasks yet' : 'No projects yet'}</p>
      <div className="sidebar-bottom"><button className={`nav-row ${page === 'settings' ? 'selected' : ''}`} onClick={() => setPage('settings')}><Settings /><span>Settings</span></button><span className="version">v0.1.3 · UI preview</span></div>
    </aside>
    </div>
    <div className="sidebar-resizer" inert={modal ? true : undefined} role="separator" tabIndex={0} aria-label="Resize sidebar" aria-orientation="vertical" aria-controls="sidebar-content" aria-valuemin={0} aria-valuemax={sidebarLimit} aria-valuenow={sidebar ? Math.round(sidebarWidth) : 0} aria-valuetext={sidebar ? `${Math.round(sidebarWidth)} pixels. Drag farther left to hide.` : 'Sidebar hidden. Drag right to show.'} title={sidebar ? 'Drag to resize. Keep dragging left to hide the sidebar.' : 'Drag right to show the sidebar.'}
      onPointerDown={event => {
        if (event.button !== 0) return;
        event.preventDefault(); event.currentTarget.focus();
        event.currentTarget.setPointerCapture(event.pointerId);
        sidebarDrag.current = { pointer: event.pointerId, x: event.clientX, width: sidebar ? sidebarWidth : 0, startedOpen: sidebar, open: sidebar };
        setResizingSidebar(true);
      }}
      onPointerMove={moveSidebar}
      onPointerUp={event => { if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId); finishSidebarResize(); }}
      onPointerCancel={finishSidebarResize} onLostPointerCapture={finishSidebarResize}
      onDoubleClick={() => { setSidebar(true); resizeSidebar(sidebarDefault()); }}
      onKeyDown={event => {
        if (!['ArrowLeft', 'ArrowRight', 'Home', 'End', 'Enter'].includes(event.key)) return;
        event.preventDefault();
        if (event.key === 'Enter') { setSidebar(!sidebar); return; }
        if (event.key === 'ArrowLeft' && sidebarWidth <= SIDEBAR_MIN) { setSidebar(false); return; }
        if (event.key === 'ArrowLeft' && !sidebar) return;
        if (event.key === 'ArrowRight' && !sidebar) { setSidebar(true); resizeSidebar(SIDEBAR_MIN); return; }
        setSidebar(true);
        resizeSidebar(({ ArrowLeft: sidebarWidth - 10, ArrowRight: sidebarWidth + 10, Home: SIDEBAR_MIN, End: sidebarLimit })[event.key]);
      }} />

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
              {files.length > 0 && <div className="attachment-chips">{files.map(file => <span className="attachment-chip" key={file.id}><FileCode2 /><span>{file.name}</span><IconButton label={`Remove file ${file.name}`} onClick={() => removeFile(file.id)}><X /></IconButton></span>)}</div>}
              <div className="composer-tools">
                <div className="composer-options">
                  <IconButton label="Attachments — preview" onClick={() => openModal('attachments')}><Plus /></IconButton>
                  <span className="separator" />
                  <button className="model-picker" onClick={() => openModal('models')}>No model connected<ChevronDown /></button>
                  <button className="context-picker" aria-label={project ? `Project: ${project.name}` : 'Choose project'} onClick={() => openModal('projects')}><Folder /><span>{project?.name ?? 'Choose project'}</span><ChevronDown /></button>
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
            {settingTab === 'About' && <><Mark className="about-mark" /><h2>rozu</h2><p className="section-description">A little curiosity. A lot of possibility.</p><div className="setting-row"><span>Version</span><span>0.1.3</span></div><div className="setting-row"><span>Build</span><span>Interface preview</span></div><div className="setting-row"><span>License</span><span>MIT</span></div><p className="settings-footnote">Frontend only. No AI calls, command execution, connected projects, or background jobs.</p></>}
          </div></div></div>}
        </main>

        {panel && <aside className="inspector" aria-label="Workspace panel"><div className="inspector-tabs">{['Files', 'Changes', 'Terminal'].map(tab => <button key={tab} className={panelTab === tab ? 'active' : ''} onClick={() => setPanelTab(tab)}>{tab}</button>)}</div>{panelTab === 'Files' && files.length > 0 ? <div className="preview-files"><FileEntries files={files} onRemove={removeFile} /><p>Preview entries · no files connected</p></div> : <div className="inspector-empty">{panelTab === 'Terminal' ? <Terminal /> : panelTab === 'Files' ? <Folder /> : <FileCode2 />}<h3>{panelTab === 'Terminal' ? 'No terminal session' : panelTab === 'Files' ? 'No files opened' : 'No changes to review'}</h3><p>{panelTab === 'Terminal' ? 'Command execution is not connected.' : panelTab === 'Files' ? 'Your project files will appear here.' : 'Future code changes will appear here.'}</p><span className="subtle-label">Interface preview</span>{panelTab === 'Files' && <button className="secondary-button" onClick={() => openModal('attachments')}><Plus />Add file entry</button>}</div>}</aside>}
      </div>
    </section>

    {modal && <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) closeModal(); }}><section ref={dialogRef} role="dialog" aria-modal="true" aria-labelledby="dialog-title" className={`modal ${modal === 'search' ? 'search-modal' : ''}`}>
      {modal === 'search' ? <><div className="search-input"><Search /><input aria-label="Search tasks" placeholder="Search tasks…" value={query} onChange={event => setQuery(event.target.value)} /><button className="escape" onClick={closeModal}>Esc</button></div><h2 id="dialog-title" className="visually-hidden">Search tasks</h2><div className="search-empty"><MessageSquare /><h3>{query ? 'No matching tasks' : 'No tasks to search yet'}</h3><p>Tasks will appear here once Rozu is connected.</p></div></> : <><div className="modal-heading"><h2 id="dialog-title">{({projects:'Projects',models:'Choose a model',attachments:'Add context',environment:'Environment',automation:'New automation',skill:'Add skill'})[modal]}</h2><IconButton label="Close dialog" onClick={closeModal}><X /></IconButton></div>
        {modal === 'projects' && <><p>Projects in this preview. Removing one only removes it from this list.</p>
          {projects.map(item => <div className="project-choice" key={item.id}><button className="choice-row" onClick={() => { setProjectId(item.id); setPage('task'); closeModal(); }}><Folder /><span>{item.name}<small>Preview project · no folder connected</small></span>{projectId === item.id && <Check />}</button><IconButton label={`Remove project ${item.name}`} onClick={() => removeProject(item.id)}><X /></IconButton></div>)}
          {!projects.length && <p>No projects yet.</p>}
          <form onSubmit={addProject}><label className="field">Project name<input value={projectName} onChange={event => setProjectName(event.target.value)} placeholder="e.g. My project" maxLength={100} /></label><button className="secondary-button" disabled={!projectName.trim()}><Plus />Add preview project</button></form>
        </>}
        {modal === 'models' && <><div className="dialog-empty"><SlidersHorizontal /><h3>No model connected</h3><p>Model selection will be available after you connect a provider.</p></div><button className="secondary-button" onClick={() => { setPage('settings'); setSettingTab('Models'); closeModal(); }}>View model settings</button></>}
        {modal === 'environment' && <><button className="choice-row" onClick={closeModal}><Monitor /><span>Local<small>Interface only · no local execution</small></span><Check /></button><p>Environment connections are not available in this preview.</p></>}
        {modal === 'attachments' && <><p>Try adding and removing file entries. These are names only; no files are read or deleted.</p><FileEntries files={files} onRemove={removeFile} />
          <form onSubmit={addFile}><label className="field">File name<input value={fileName} onChange={event => setFileName(event.target.value)} placeholder="e.g. notes.md" maxLength={160} /></label><button className="secondary-button" disabled={!fileName.trim()}><Plus />Add file entry</button></form>
        </>}
        {modal === 'automation' && <><label className="field">Name<input placeholder="e.g. Review recent changes" /></label><label className="field">Instructions<textarea placeholder="What should Rozu do?" /></label><div className="form-bottom"><span>Scheduling is not connected.</span><button className="secondary-button" disabled>Create automation</button></div></>}
        {modal === 'skill' && <><label className="field">Name<input placeholder="e.g. Code review" /></label><label className="field">Instructions<textarea placeholder="Describe how this skill should work…" /></label><div className="form-bottom"><span>Skills are not saved in this preview.</span><button className="secondary-button" disabled>Add skill</button></div></>}
      </>}
    </section></div>}
    <span className="visually-hidden" role="status">{notice}</span>
  </div>;
}

function FileEntries({ files, onRemove }) {
  return <ul className="file-entries">{files.map(file => <li key={file.id}><FileCode2 /><span>{file.name}</span><IconButton label={`Remove file ${file.name}`} onClick={() => onRemove(file.id)}><X /></IconButton></li>)}</ul>;
}

function PageContent({ title, description, icon: Icon, empty, detail, button, onClick }) {
  return <div className="collection-page"><div className="collection-header"><div><h1>{title}</h1><p>{description}</p></div>{button && <button className="secondary-button" onClick={onClick}><Plus />{button}</button>}</div><div className="collection-empty"><Icon /><h2>{empty}</h2><p>{detail}</p><span className="subtle-label">Interface preview</span></div></div>;
}

createRoot(document.getElementById('root')).render(<App />);
