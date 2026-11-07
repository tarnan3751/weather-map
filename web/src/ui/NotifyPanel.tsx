import { useEffect, useState } from 'react';
import { push as pushApi } from '../api/device.ts';
import { currentSubscription, disablePush, enablePush, isIos, PushError, pushBlocker, resyncPush, type PushBlocker } from '../app/push.ts';
import { useApp, useAppState } from './context.ts';
import { Sheet } from './Sheet.tsx';

const BLOCKER_TEXT: Record<PushBlocker, { title: string; body: string }> = {
  insecure: {
    title: 'Needs a secure connection',
    body: 'Browsers only allow notifications on sites served over HTTPS (or from this computer). Open the app over HTTPS to turn them on.',
  },
  unsupported: {
    title: 'This browser cannot receive notifications',
    body: 'It does not support Web Push. Recent Chrome, Edge, Firefox and Safari can; on an iPhone, add this site to the Home Screen first.',
  },
  'ios-needs-home-screen': {
    title: 'Add to Home Screen first',
    body: 'On iPhone and iPad, Safari only allows notifications for web apps on the Home Screen. Tap the Share button, choose “Add to Home Screen”, then open the app from there and come back here.',
  },
  'ios-too-old': {
    title: 'Update iOS to get notifications',
    body: 'Web notifications need iOS 16.4 or later. Update in Settings › General › Software Update.',
  },
  'no-server-key': {
    title: 'Notifications are not set up on this server',
    body: 'The server has no push key configured, so it cannot send notifications. Alerts still appear on the map.',
  },
  denied: {
    title: 'Notifications are blocked',
    body: 'This site was blocked from showing notifications. Allow them in your browser’s site settings (the icon left of the address), then reload.',
  },
};

type Status = { state: 'checking' } | { state: 'blocked'; why: PushBlocker } | { state: 'off' } | { state: 'on' };

export function NotifyPanel() {
  const { store } = useApp();
  const vapid = useAppState((s) => s.config?.vapid_public_key ?? null);
  const places = useAppState((s) => s.places);
  const [status, setStatus] = useState<Status>({ state: 'checking' });
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    const why = pushBlocker(vapid);
    if (why) setStatus({ state: 'blocked', why });
    // Re-send (and if the key changed, renew) the subscription first, so "On"
    // means the server can actually reach this device.
    else void resyncPush(vapid).then(currentSubscription).then((sub) => live && setStatus(sub ? { state: 'on' } : { state: 'off' }));
    return () => {
      live = false;
    };
  }, [vapid]);

  const enable = async () => {
    if (!vapid) return;
    setBusy(true);
    setMessage(null);
    try {
      await enablePush(vapid);
      setStatus({ state: 'on' });
      setMessage('Notifications are on for this device.');
    } catch (e) {
      const err = e as PushError;
      if (err.reason === 'denied') setStatus({ state: 'blocked', why: 'denied' });
      else if (err.reason === 'dismissed') setMessage('Notifications were not allowed. You can try again.');
      else if (err.reason === 'subscribe') setMessage(`The browser could not subscribe: ${err.message}`);
      else setMessage('The server did not accept this device. Try again in a moment.');
    } finally {
      setBusy(false);
    }
  };

  const disable = async () => {
    setBusy(true);
    try {
      await disablePush();
      setStatus({ state: 'off' });
      setMessage('Notifications are off for this device.');
    } catch {
      setMessage('Could not turn notifications off.');
    } finally {
      setBusy(false);
    }
  };

  const test = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const r = await pushApi.test();
      setMessage(
        r.sent > 0
          ? `Test sent to ${r.sent} ${r.sent === 1 ? 'device' : 'devices'}${r.failed ? `; ${r.failed} failed` : ''}. It should arrive in a few seconds.`
          : 'The server has no working subscription for this device. Turn notifications off and on again.',
      );
    } catch {
      setMessage('The test could not be sent.');
    } finally {
      setBusy(false);
    }
  };

  const notifying = places.filter((p) => p.notify).length;

  return (
    <Sheet title="Notifications" labelId="notify-title">
      <p className="lead">
        Get a notification when the National Weather Service issues a warning that covers one of your saved places, even when this tab is closed.
      </p>
      {status.state === 'checking' && <div className="skeleton short" aria-busy="true" />}
      {status.state === 'blocked' && (
        <div className="notice">
          <h3>{BLOCKER_TEXT[status.why].title}</h3>
          <p>{BLOCKER_TEXT[status.why].body}</p>
        </div>
      )}
      {status.state === 'off' && (
        <div className="notify-actions">
          <button className="btn primary" onClick={() => void enable()} disabled={busy} data-autofocus>
            Enable notifications
          </button>
          <p className="muted small">Your browser will ask for permission.</p>
        </div>
      )}
      {status.state === 'on' && (
        <div className="notify-actions">
          <p className="status-line">
            <span className="status-dot ok" aria-hidden="true" /> On for this device
          </p>
          <div className="row">
            <button className="btn" onClick={() => void test()} disabled={busy}>
              Send a test notification
            </button>
            <button className="btn ghost" onClick={() => void disable()} disabled={busy}>
              Turn off
            </button>
          </div>
        </div>
      )}
      {message && (
        <p className="form-note" role="status">
          {message}
        </p>
      )}
      <div className="notice subtle">
        <p>
          {places.length === 0
            ? 'You have no saved places yet. Tap the map and choose Save place.'
            : places.length === 1
              ? notifying
                ? `Your saved place, ${places[0].name}, has notifications on.`
                : `Your saved place, ${places[0].name}, has notifications off.`
              : notifying
                ? `${notifying} of your ${places.length} saved places have notifications on.`
                : 'None of your saved places has notifications on.'}
        </p>
        <button className="btn small" onClick={() => store.set({ panel: { kind: 'places' } })}>
          Saved places
        </button>
      </div>
      {isIos() && status.state !== 'blocked' && <p className="muted small">On iPhone, notifications arrive while the Home Screen app is installed.</p>}
    </Sheet>
  );
}
