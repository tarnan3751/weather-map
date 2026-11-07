// Service worker: shows warning notifications pushed by the server and opens
// the app at the alert when one is tapped. It does not cache anything; the map
// is live data and should never be served stale.

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

self.addEventListener('push', (event) => {
  let p = {};
  try {
    p = event.data ? event.data.json() : {};
  } catch {
    p = { title: 'Weather alert', body: event.data ? event.data.text() : '' };
  }
  const title = p.title || 'Weather alert';
  const extreme = p.severity === 'Extreme';
  event.waitUntil(
    self.registration.showNotification(title, {
      body: p.body || '',
      // Updates to the same hazard replace the earlier notification.
      tag: p.tag || p.alert_id || undefined,
      renotify: Boolean(p.tag),
      requireInteraction: extreme,
      icon: '/icons/icon-192.png',
      badge: '/icons/badge-72.png',
      data: { url: p.url || '/', alert_id: p.alert_id || null },
    }),
  );
});

self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  const target = new URL((event.notification.data && event.notification.data.url) || '/', self.location.origin).href;
  event.waitUntil(openAlert(target));
});

async function openAlert(target) {
  const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  const ours = windows.filter((c) => new URL(c.url).origin === self.location.origin);
  // The map, if it is open: it flies to the alert without reloading.
  const map = ours.find((c) => new URL(c.url).pathname === '/');
  if (map) {
    map.postMessage({ type: 'open-alert', url: target });
    return map.focus();
  }
  // Another page of ours (the status page): turn it into the map at the alert.
  for (const c of ours) {
    try {
      const moved = await c.navigate(target);
      if (moved) return moved.focus();
    } catch {
      // Not controlled by this worker yet; open a new window instead.
    }
  }
  return self.clients.openWindow(target);
}
