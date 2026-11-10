import re, sys
src, dst, theme, scene = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
s = open(src).read()
v = {'themeClass': 'th-'+theme, 'theme': theme,
     'tbStyle': 'background: linear-gradient(90deg,#053a2a,#127a55)',
     'logoW': '660', 'lOn': 'pressed', 'mOn': '', 'sOn': '',
     'lPressed': 'true', 'mPressed': 'false', 'sPressed': 'false',
     'attached': 'false', 'notAttached': 'true', 'helpOpen': 'false', 'helpOn': '', 'helpExpanded': 'false',
     'true': 'true', 'false': 'false',
     'isMorning': str(scene=='morning').lower(), 'isMidday': str(scene=='midday').lower(),
     'isDusk': str(scene=='dusk').lower(), 'isNight': str(scene=='night').lower(),
     'sceneLabel': scene.title(), 'clock': {'morning':'7:42 AM','midday':'12:15 PM','dusk':'6:20 PM','night':'10:05 PM'}.get(scene,'10:05 PM'),
     'greeting': {'morning':'Good morning','midday':'Good afternoon','dusk':'Good evening','night':'Good night'}.get(scene,'Good night')}
def ifblock(m):
    return m.group(2) if v.get(m.group(1)) == 'true' else ''
for _ in range(3):
    s = re.sub(r'<sc-if value="\{\{(\w+)\}\}"[^>]*>(.*?)</sc-if>', ifblock, s, flags=re.S)
s = re.sub(r'\{\{(\w+)\}\}', lambda m: v.get(m.group(1), ''), s)
s = s.replace('<script src="./support.js">', '<script>')
s = re.sub(r'<select([^>]*)>', r'<select\1>', s)
s = s.replace('<option value="%s">' % theme, '<option value="%s" selected>' % theme)
open(dst, 'w').write(s)
