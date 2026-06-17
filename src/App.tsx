import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
// @ts-ignore: listen types
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import "./App.css";

type AppSettings = {
  bearerToken: string;
  defaultSavePath: string;
};

type DownloadType = "magnet" | "torrent";

type StartDownloadRequest = {
  magnetLink?: string;
  torrentFileName?: string;
  torrentFileBytes?: number[];
  destinationPath?: string;
  suggestedFileName?: string;
  allowZip?: boolean;
  asQueued?: boolean;
};

type StartLinkRequest = {
  magnetLink?: string;
  torrentFileName?: string;
  torrentFileBytes?: number[];
  suggestedFileName?: string;
  allowZip?: boolean;
  asQueued?: boolean;
};

type LinkRequestResult = {
  torrentId: string;
  downloadUrl: string;
  detail: string;
};

type DownloadResult = {
  torrentId: string;
  outputPath: string;
  bytesWritten: number;
  detail: string;
};

type RequestEntry = {
  id: string;
  status: "in-progress" | "completed" | "failed";
  statusMessage: string;
  result: DownloadResult | LinkRequestResult | null;
  createdAt: number;
  bytesDownloaded: number;
  totalBytes: number;
};

type ProgressPayload = {
  bytesDownloaded: number;
  totalBytes: number | null;
};

// Helper to format bytes to human-readable format
function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + " " + sizes[i];
}

function App() {
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const downloadCounterRef = useRef(0);

  const [settings, setSettings] = useState<AppSettings>({
    bearerToken: "",
    defaultSavePath: "",
  });
  const [settingsPanelOpen, setSettingsPanelOpen] = useState(false);

  const [downloadType, setDownloadType] = useState<DownloadType>("magnet");
  const [magnetLink, setMagnetLink] = useState("");
  const [torrentFileName, setTorrentFileName] = useState("");
  const [torrentFileBytes, setTorrentFileBytes] = useState<number[] | undefined>();
  const [suggestedFileName, setSuggestedFileName] = useState("");
  const [destinationFolder, setDestinationFolder] = useState("");

  const [isWorking, setIsWorking] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [downloads, setDownloads] = useState<RequestEntry[]>([]);

  // Hydrate settings on mount
  useEffect(() => {
    let cancelled = false;
    let unlistenStatus: (() => void) | null = null;
    let unlistenProgress: (() => void) | null = null;

    async function hydrateSettings() {
      try {
        const savedSettings = await invoke<AppSettings>("load_settings");
        if (!cancelled) {
          setSettings(savedSettings);
          setDestinationFolder(savedSettings.defaultSavePath);
        }
      } catch (error) {
        // ignore
      }
    }

    void hydrateSettings();

    // listen for backend progress/log events
    void (async () => {
      try {
        // Handle text status messages
        const unStatus = await listen<string>("download-status", (event) => {
          const text = String(event.payload);
          setDownloads((cur) => {
            const last = cur[cur.length - 1];
            if (last && last.status === "in-progress") {
              return [
                ...cur.slice(0, -1),
                { ...last, statusMessage: text },
              ];
            }
            return cur;
          });
        });

        // Handle numeric byte progress payload
        const unProgress = await listen<ProgressPayload>("download-progress", (event) => {
          const { bytesDownloaded, totalBytes } = event.payload;
          setDownloads((cur) => {
            const last = cur[cur.length - 1];
            if (last && last.status === "in-progress") {
              return [
                ...cur.slice(0, -1),
                {
                  ...last,
                  bytesDownloaded,
                  totalBytes: totalBytes ?? last.totalBytes, // Fallback if totalBytes is null
                },
              ];
            }
            return cur;
          });
        });

        unlistenStatus = unStatus as unknown as () => void;
        unlistenProgress = unProgress as unknown as () => void;
      } catch (_) {
        // ignore
      }
    })();

    return () => {
      cancelled = true;
      if (unlistenStatus) unlistenStatus();
      if (unlistenProgress) unlistenProgress();
    };
  }, []);

  async function chooseDestinationFolder() {
    try {
      const path = await open({ directory: true });
      if (typeof path === "string" && path.trim()) {
        setDestinationFolder(path);
      }
    } catch (e) {
      // ignore
    }
  }

  async function saveSettings() {
    setIsSaving(true);
    try {
      const updatedSettings = { ...settings, defaultSavePath: destinationFolder };
      const savedSettings = await invoke<AppSettings>("save_settings", {
        settings: updatedSettings,
      });
      setSettings(savedSettings);
      setDestinationFolder(savedSettings.defaultSavePath);
    } catch (error) {
      // ignore
    } finally {
      setIsSaving(false);
    }
  }

  async function onTorrentFileSelected(file: File | null) {
    if (!file) {
      setTorrentFileName("");
      setTorrentFileBytes(undefined);
      return;
    }

    const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
    setTorrentFileName(file.name);
    setTorrentFileBytes(bytes);
  }

  async function startRequestLink() {
    if (!settings.bearerToken.trim()) {
      alert("Please configure your bearer token in settings.");
      return;
    }

    const hasInput = downloadType === "magnet" ? magnetLink.trim() : torrentFileBytes;
    if (!hasInput) {
      alert(`Please provide a ${downloadType} link or file.`);
      return;
    }

    const downloadId = `dl-${++downloadCounterRef.current}`;
    const requestEntry: RequestEntry = {
      id: downloadId,
      status: "in-progress",
      statusMessage: "Starting request...",
      result: null,
      createdAt: Date.now(),
      bytesDownloaded: 0,
      totalBytes: 0,
    };

    setDownloads((cur) => [...cur, requestEntry]);
    setIsWorking(true);

    const payload: StartLinkRequest = {
      magnetLink: downloadType === "magnet" ? magnetLink.trim() || undefined : undefined,
      torrentFileName: downloadType === "torrent" ? torrentFileName || undefined : undefined,
      torrentFileBytes: downloadType === "torrent" ? torrentFileBytes : undefined,
      suggestedFileName: suggestedFileName.trim() || undefined,
      allowZip: true,
      asQueued: false,
    };

    try {
      const result = await invoke<LinkRequestResult>("start_link_request", {
        request: payload,
      });

      setDownloads((cur) =>
        cur.map((d) =>
          d.id === downloadId
            ? {
                ...d,
                status: "completed",
                result,
                statusMessage: result.detail,
              }
            : d
        )
      );

      // Reset form
      setMagnetLink("");
      setTorrentFileName("");
      setTorrentFileBytes(undefined);
      setSuggestedFileName("");
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : "Download failed.";
      setDownloads((cur) =>
        cur.map((d) =>
          d.id === downloadId
            ? {
                ...d,
                status: "failed",
                statusMessage: errorMessage,
              }
            : d
        )
      );
    } finally {
      setIsWorking(false);
    }
  }

  async function startDownload() {
    if (!settings.bearerToken.trim()) {
      alert("Please configure your bearer token in settings.");
      return;
    }

    const hasInput = downloadType === "magnet" ? magnetLink.trim() : torrentFileBytes;
    if (!hasInput) {
      alert(`Please provide a ${downloadType} link or file.`);
      return;
    }

    const downloadId = `dl-${++downloadCounterRef.current}`;
    const requestEntry: RequestEntry = {
      id: downloadId,
      status: "in-progress",
      statusMessage: "Starting download...",
      result: null,
      createdAt: Date.now(),
      bytesDownloaded: 0,
      totalBytes: 0,
    };

    setDownloads((cur) => [...cur, requestEntry]);
    setIsWorking(true);

    const payload: StartDownloadRequest = {
      magnetLink: downloadType === "magnet" ? magnetLink.trim() || undefined : undefined,
      torrentFileName: downloadType === "torrent" ? torrentFileName || undefined : undefined,
      torrentFileBytes: downloadType === "torrent" ? torrentFileBytes : undefined,
      destinationPath: destinationFolder.trim() || undefined,
      suggestedFileName: suggestedFileName.trim() || undefined,
      allowZip: true,
      asQueued: false,
    };

    try {
      const result = await invoke<DownloadResult>("start_download", {
        request: payload,
      });

      setDownloads((cur) =>
        cur.map((d) =>
          d.id === downloadId
            ? {
                ...d,
                status: "completed",
                result,
                statusMessage: result.detail,
              }
            : d
        )
      );

      // Reset form
      setMagnetLink("");
      setTorrentFileName("");
      setTorrentFileBytes(undefined);
      setSuggestedFileName("");
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : "Download failed.";
      setDownloads((cur) =>
        cur.map((d) =>
          d.id === downloadId
            ? {
                ...d,
                status: "failed",
                statusMessage: errorMessage,
              }
            : d
        )
      );
    } finally {
      setIsWorking(false);
    }
  }

  return (
    <div className="app-container">
      {/* Header with Settings Button */}
      <div className="header-wrapper">
        <header className="app-header">
          <div className="header-content">
            <div>
              <h1 className="app-title">Torbox Download Client</h1>
            </div>
          </div>
        </header>
        
        <button
          className="settings-toggle"
          onClick={() => setSettingsPanelOpen(!settingsPanelOpen)}
          aria-label={settingsPanelOpen ? "Close settings" : "Open settings"}
        >
          {settingsPanelOpen ? "✕" : "⚙"}
        </button>
      </div>

      {/* Settings Panel (Fold out from top right) */}
      <div className={`settings-panel ${settingsPanelOpen ? "open" : "closed"}`}>
        <div className="settings-content">
          <h2>Settings</h2>
          <p className="settings-subtitle">Configure your Torbox API credentials and preferences</p>

          <label className="field">
            <span>Bearer token</span>
            <input
              type="password"
              value={settings.bearerToken}
              onChange={(event) => setSettings((current) => ({ ...current, bearerToken: event.target.value }))}
              placeholder="Paste your Torbox API token"
            />
          </label>

          <label className="field">
            <span>Default save path</span>
            <div className="path-input-row">
              <input
                type="text"
                value={destinationFolder}
                onChange={(event) => setDestinationFolder(event.target.value)}
                placeholder="C:\\Users\\you\\Downloads\\Torbox"
              />
              <button className="secondary-button" type="button" onClick={chooseDestinationFolder}>
                Choose...
              </button>
            </div>
          </label>

          <button className="primary-button" type="button" onClick={saveSettings} disabled={isSaving}>
            {isSaving ? "Saving..." : "Save settings"}
          </button>
        </div>
      </div>

      {/* Main Content Area */}
      <main className="main-content">
        {/* Left Column: Previous Downloads */}
        <aside className="downloads-history">
          <h2>Download History</h2>
          {downloads.length === 0 ? (
            <div className="empty-state">No downloads yet. Start one on the right.</div>
          ) : (
            <div className="downloads-list">
              {[...downloads].reverse().map((dl) => (
                <div key={dl.id} className={`download-card status-${dl.status}`}>
                  <div className="download-header">
                    <div className="status-badge">{dl.status}</div>
                    <span className="download-time">
                      {new Date(dl.createdAt).toLocaleTimeString()}
                    </span>
                  </div>
                  <div className="download-message">{dl.statusMessage}</div>
                  
                  {/* In-Progress: Show progress bar and current/total size */}
                  {dl.status === "in-progress" && (
                    <div className="progress-bar-container">
                      <div className="progress-bar">
                        <div 
                          className="progress-fill" 
                          style={{ 
                            width: dl.totalBytes > 0 ? `${(dl.bytesDownloaded / dl.totalBytes) * 100}%` : "0%" 
                          }}
                        />
                      </div>
                      <div className="progress-text">
                        {formatBytes(dl.bytesDownloaded)} / {dl.totalBytes > 0 ? formatBytes(dl.totalBytes) : "..."}
                      </div>
                    </div>
                  )}
                  
                  {dl.result && "bytesWritten" in dl.result && (
                    <div className="download-details">
                      <div className="detail-row">
                        <span className="label">Torrent ID:</span>
                        <span className="value">{dl.result.torrentId}</span>
                      </div>
                      <div className="detail-row">
                        <span className="label">Size:</span>
                        <span className="value">{formatBytes(dl.result.bytesWritten)}</span>
                      </div>
                      <div className="detail-row">
                        <span className="label">Path:</span>
                        <span className="value path">{dl.result.outputPath}</span>
                      </div>
                    </div>
                  )}

                  {dl.result && "downloadUrl" in dl.result && (
                    <div className="download-details">
                      <div className="detail-row">
                        <span className="label">Download URL:</span>
                        <div className="actions-row">
                          <button
                            className="secondary-button"
                            onClick={() => {
                              navigator.clipboard.writeText((dl.result as LinkRequestResult).downloadUrl);
                            }}
                          >
                            Copy
                          </button>
                          <button
                            className="secondary-button"
                            onClick={() => {
                              openUrl((dl.result as LinkRequestResult).downloadUrl);
                            }}
                          >
                            Open
                          </button>
                        </div>
                      </div>
                      <div className="detail-row">
                        <span className="label">Detail:</span>
                        <span className="value">{dl.result.detail}</span>
                      </div>
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </aside>

        {/* Right Column: Download Form */}
        <section className="download-form-area">
          <h2>New Download</h2>

          {/* Download Type Selector */}
          <div className="type-selector">
            <label className="field">
              <span>Download type</span>
              <select
                value={downloadType}
                onChange={(event) => {
                  setDownloadType(event.target.value as DownloadType);
                  setMagnetLink("");
                  setTorrentFileName("");
                  setTorrentFileBytes(undefined);
                }}
                className="type-select"
              >
                <option value="magnet">Magnet Link</option>
                <option value="torrent">Torrent File</option>
              </select>
            </label>
          </div>

          {/* Magnet or Torrent Input */}
          {downloadType === "magnet" ? (
            <label className="field">
              <span>Magnet link</span>
              <textarea
                value={magnetLink}
                onChange={(event) => setMagnetLink(event.target.value)}
                placeholder="magnet:?xt=urn:btih:..."
                rows={4}
              />
            </label>
          ) : (
            <div className="field">
              <span>Torrent file</span>
              <div className="file-row">
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".torrent,application/x-bittorrent"
                  onChange={async (event) => {
                    await onTorrentFileSelected(event.target.files?.[0] ?? null);
                  }}
                  hidden
                />
                <button
                  className="secondary-button"
                  type="button"
                  onClick={() => fileInputRef.current?.click()}
                >
                  {torrentFileName ? "Replace file" : "Choose file"}
                </button>
                <span className="file-name">{torrentFileName || "No file selected"}</span>
              </div>
            </div>
          )}

          {/* Destination Folder */}
          <div className="field">
            <span>Destination folder</span>
            <div className="path-input-row">
              <input
                type="text"
                value={destinationFolder}
                onChange={(event) => setDestinationFolder(event.target.value)}
                placeholder="C:\\Users\\you\\Downloads\\Torbox"
              />
              <button className="secondary-button" type="button" onClick={chooseDestinationFolder}>
                Browse...
              </button>
            </div>
          </div>

          {/* Optional Filename */}
          <label className="field">
            <span>Optional filename</span>
            <input
              value={suggestedFileName}
              onChange={(event) => setSuggestedFileName(event.target.value)}
              placeholder="Leave blank to auto-detect"
            />
          </label>

          <div className="actions-row">
            {/* Start Download Button */}
            <button
              className="primary-button start-download-btn"
              type="button"
              onClick={startDownload}
              disabled={isWorking}
            >
              {isWorking ? "Working..." : "Start Download"}
            </button>

            {/* Start Link Request Button */}
            <button
              className="primary-button start-download-btn"
              type="button"
              onClick={startRequestLink}
              disabled={isWorking}
            >
              {isWorking ? "..." : "Get Link"}
            </button>
          </div>
        </section>
      </main>
    </div>
  );
}

export default App;
