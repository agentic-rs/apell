import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { AppletIcon, appletIconSource } from './AppletIcon';
import companion_icon from '../../assets/companion.svg';

const package_icon = 'data:image/png;base64,cGFja2FnZQ==';
const applet = { applet_id: 'example', name: 'Example', icon_data_url: package_icon };

describe('installed applet icons', () => {
  it('uses package artwork for any applet, including Companion', () => {
    expect(appletIconSource(applet)).toBe(package_icon);
    expect(appletIconSource({ ...applet, applet_id: 'chatbot' })).toBe(package_icon);
    expect(renderToStaticMarkup(<AppletIcon applet={applet} />)).toContain(`src="${package_icon}"`);
  });

  it('keeps the legacy Companion icon when package artwork is missing or fails', () => {
    const companion = { ...applet, applet_id: 'chatbot' };
    const fallback = appletIconSource({ ...companion, icon_data_url: null });
    expect(fallback).toBe(companion_icon);
    expect(appletIconSource(companion, [package_icon])).toBe(fallback);
    expect(appletIconSource(companion, [package_icon, fallback!])).toBeNull();
  });

  it('falls back to initials without artwork and retries a changed package icon', () => {
    expect(appletIconSource(applet, [package_icon])).toBeNull();
    expect(
      renderToStaticMarkup(<AppletIcon applet={{ applet_id: 'example', name: ' Example ' }} />),
    ).toContain('<span class="applet-initial">E</span>');
    expect(
      renderToStaticMarkup(<AppletIcon applet={{ applet_id: 'example', name: ' ' }} />),
    ).toContain('<span class="applet-initial">a</span>');
    const replacement = 'data:image/png;base64,bmV3';
    expect(appletIconSource({ ...applet, icon_data_url: replacement }, [package_icon])).toBe(
      replacement,
    );
  });
});
