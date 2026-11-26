import type { ComponentProps } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { SystemPermissionAccess } from './SystemPermissionAccess';

function render(overrides: Partial<ComponentProps<typeof SystemPermissionAccess>> = {}) {
  return renderToStaticMarkup(
    <SystemPermissionAccess
      id="microphone-system"
      applet_name="Recording fixture"
      permission="microphone"
      authorization="blocked"
      mode="ask"
      can_open_system_settings
      disabled={false}
      {...overrides}
    />,
  );
}

describe('system permission access', () => {
  it('offers to open Settings for an applicable system block', () => {
    const markup = render();
    expect(markup).toContain('Blocked by system');
    expect(markup).toContain('>Open system settings</button>');
    expect(markup).toContain('aria-describedby="microphone-system"');
  });

  it('offers Android Settings when system access is needed', () => {
    const markup = render({ authorization: 'not_granted', mode: 'remembered' });
    expect(markup).toContain('System access needed');
    expect(markup).toContain('>Open system settings</button>');
  });

  it('keeps the system status separately from a denial in Apell', () => {
    const markup = render({ mode: 'blocked' });
    expect(markup).toContain('Blocked by system');
    expect(markup).not.toContain('<button');
  });

  it('shows no action when native Settings cannot help', () => {
    expect(render({ can_open_system_settings: false })).not.toContain('<button');
    expect(render({ authorization: 'not_determined' })).toContain(
      'System permission not requested',
    );
    expect(render({ authorization: 'not_determined' })).not.toContain('<button');
    expect(render({ authorization: 'restricted' })).toContain('Restricted by system');
    expect(render({ authorization: 'restricted' })).not.toContain('<button');
    expect(render({ authorization: 'allowed' })).toBe('');
    expect(render({ authorization: 'unknown' })).toBe('');
  });
});
