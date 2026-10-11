from PIL import Image, ImageDraw, ImageFont
import math, random
FR="/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"
FB="/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf"
F=lambda s,b=False: ImageFont.truetype(FB if b else FR,s)
PTS=[(0,0),(0,17),(4.5,13),(8,19.5),(10.5,17.5),(8,12.5),(12.5,12.5)]
SS=4
def arrow(img,x,y,h,fill=(255,255,255),outline=(0,0,0)):
    # supersampled arrow drawn on RGBA img at tip (x,y), height h px
    s=h/19.5
    big=Image.new("RGBA",(int(14*s*SS)+8*SS,int(21*s*SS)+8*SS),(0,0,0,0))
    d=ImageDraw.Draw(big)
    p=[(4*SS+px*s*SS,4*SS+py*s*SS) for px,py in PTS]
    d.polygon(p,fill=fill)
    d.line(p+[p[0]],fill=outline,width=max(1,int(0.9*s*SS)),joint="curve")
    sm=big.resize((big.width//SS,big.height//SS),Image.LANCZOS)
    img.alpha_composite(sm,(int(x-4),int(y-4)))
def desk(w=960,h=540):
    im=Image.new("RGBA",(w,h))
    d=ImageDraw.Draw(im)
    for yy in range(h):
        t=yy/h; d.line([(0,yy),(w,yy)],fill=(int(20+30*t),int(40+50*t),int(90+80*t),255))
    # window
    d.rounded_rectangle((120,70,640,380),8,fill=(40,40,44,255))
    d.rectangle((120,70,640,100),fill=(55,55,60,255))
    d.text((134,77),"Notes.txt - Editor",font=F(13),fill=(220,220,220))
    for i in range(7): d.rounded_rectangle((144,125+i*30,144+300-(i%3)*60,137+i*30),4,fill=(70,70,78,255))
    d.rectangle((0,h-40,w,h),fill=(24,24,28,255))
    for i in range(5): d.rounded_rectangle((16+i*44,h-32,44+i*44,h-8),6,fill=(70,75,90,255))
    d.text((w-70,h-28),"19:43",font=F(14),fill=(230,230,230))
    d.rectangle((0,0,0,0)); 
    d.text((w-330,h-30),"",font=F(10))
    return im
def tag(im,text):
    d=ImageDraw.Draw(im); w=im.width
    d.rounded_rectangle((10,10,10+d.textlength(text,font=F(13))+20,38),6,fill=(0,0,0,170))
    d.text((20,16),text,font=F(13),fill=(255,220,120))
# ---- GIF
frames=[];bg=desk()
N=64
def pos(t):
    if t<24: # shake
        return 380+110*math.sin(t*1.1)+t*3, 250+25*math.sin(t*0.5)
    if t<40: return 380+110*math.sin(24*1.1)+24*3+ (t-24)*3, 250
    return 520+(t-40)*5,250
for t in range(N):
    im=bg.copy(); x,y=pos(t)
    if t<8: size=19.5*(1+0.0*t); big=0
    # enlargement: grows from 8..14, stays until 40, shrinks 40..46
    if t<8: g=0
    elif t<14: g=(t-8)/6
    elif t<40: g=1
    elif t<46: g=1-(t-40)/6
    else: g=0
    if g<=0.01: arrow(im,x,y,20)
    else:
        arrow(im,x-(12.5/19.5)*(20+90*g)/2*1.0,y-(20+90*g)/2,20+90*g)
    label=["1. normal cursor","2. shake -> big cursor (small arrow hidden)","3. stop -> back to normal"][0 if t<8 else (1 if t<40 else 2)]
    tag(im,"MOCKUP (simulation, not a real Windows capture) - "+label)
    frames.append(im.convert("P",palette=Image.ADAPTIVE,colors=128))
frames[0].save("cursor-demo.gif",save_all=True,append_images=frames[1:],duration=60,loop=0,disposal=2)
# one still of enlarged effect
im=bg.copy(); x,y=380,260; arrow(im,x-35,y-55,110); tag(im,"MOCKUP (simulation, not a real Windows capture) - enlarged cursor"); im.convert("RGB").save("screenshot-enlarged.png")
# ---- settings window 384x244 scaled x2
S=2
def rr(d,r,rad,fill,outline=None): d.rounded_rectangle([v*S for v in r],rad*S,fill=fill,outline=outline,width=S if outline else 0)
W,H=384,244
im=Image.new("RGB",(W*S,H*S+32*S),(32,32,32)); d=ImageDraw.Draw(im)
d.rectangle((0,0,W*S,32*S),fill=(24,24,24)); d.text((12*S,8*S),"CursorFinder settings",font=F(13*S),fill=(230,230,230)); d.text((W*S-30*S,6*S),"x",font=F(16*S),fill=(200,200,200))
def Y(v): return (v+32)
d.text((24*S,Y(20)*S),"Shake detection",font=F(16*S,True),fill=(232,232,232))
for row,txt,val in [(70,"Corner max interval, ms (20-800)","200"),(112,"Corner min distance, px (10-2000)","300")]:
    d.text((24*S,Y(row+5)*S),txt,font=F(13*S),fill=(232,232,232))
    rr(d,(254,Y(row)-0,360,Y(row)+30),6,(45,45,45),(74,74,74)); d.text((264*S,(Y(row)+7)*S),val,font=F(13*S),fill=(232,232,232))
for i,(t,acc) in enumerate([("OK",1),("Apply",0),("Cancel",0)]):
    x=384-24-3*88-16+i*96
    rr(d,(x,Y(194),x+88,Y(224)),6,(0,120,212) if acc else (58,58,58),None if acc else (90,90,90))
    tw=d.textlength(t,font=F(13*S)); d.text(((x+44)*S-tw/2,(Y(194)+7)*S),t,font=F(13*S),fill=(255,255,255))
im.save("screenshot-settings.png")
# ---- tray menu
items=[("Disable while full screen",False),("Run at startup",True),("Settings...",False),None,("Quit",False)]
mw,mh=250,4+4*34+10+4
bgi=Image.new("RGBA",(420,300),(24,24,28,255)); d0=ImageDraw.Draw(bgi)
d0.rectangle((0,260,420,300),fill=(20,20,24)); d0.text((340,272),"19:43",font=F(14),fill=(230,230,230))
im=bgi.copy(); d=ImageDraw.Draw(im); x0,y0=100,260-mh-6
d.rounded_rectangle((x0,y0,x0+mw,y0+mh),0,fill=(43,43,43),outline=(68,68,68))
y=y0+4
for it in items:
    if it is None: d.line((x0+8,y+5,x0+mw-8,y+5),fill=(68,68,68)); y+=10; continue
    t,chk=it
    if t=="Settings...": d.rounded_rectangle((x0+4,y+2,x0+mw-4,y+32),5,fill=(62,62,62))
    if chk: d.text((x0+14,y+8),"v",font=F(14,True),fill=(232,232,232))
    d.text((x0+40,y+8),t,font=F(14),fill=(232,232,232)); y+=34
d.rounded_rectangle((372-110,266,392-110,286),4,fill=(0,120,212)); 
im.convert("RGB").resize((840,600),Image.LANCZOS).save("screenshot-tray-menu.png")
